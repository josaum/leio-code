#!/usr/bin/env python3
"""Model-swept LEIO benchmark over the golden context tasks, via the harness.

Generates a `leio-harness day` spec that runs one lane per (golden task x
model), where each lane's agent CLI is invoked with an OpenRouter model slug
substituted into `{model}`. The lane prompt instructs the agent to ground its
answer in the LEIO context bundle (`leio-code context`) and emit a strict JSON
verdict. After the day completes, each lane's stdout artifact is graded:

- `json_valid`      — the verdict parsed as the required JSON shape
- `evidence_ratio`  — verdict `evidence_paths` hit ratio against the task's
                      `expected_paths_any` / `expected_instruction_paths_any` /
                      `expected_memory_paths_any`
- `tests_hit`       — any of the task's `expected_tests_any` named in the
                      verdict `tests` / `answer`
- `duration_ms`     — the harness-measured lane wall clock

Results: `<out>/model-sweep-report.json` and `.md`, plus a JAI Team `widget`
block on stdout for one-click re-run from the workbench chat.

Without `--execute` the runner only writes the generated DaySpec (safe; no
agent calls, no model spend).
"""

from __future__ import annotations

import argparse
import json
import re
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

PLUGIN_ROOT = Path(__file__).resolve().parents[1]

# OpenRouter slugs used by the harness work-shape table (agents.rs).
DEFAULT_MODELS = [
    "deepseek/deepseek-v4-flash-0731",
    "openai/gpt-5.6-luna",
    "anthropic/claude-opus-5",
]

VERDICT_RESPONSE_FORMAT_SCHEMA = {
    "type": "json_schema",
    "json_schema": {
        "name": "verdict",
        "strict": True,
        "schema": {
            "type": "object",
            "properties": {
                "answer": {"type": "string"},
                "evidence_paths": {"type": "array", "items": {"type": "string"}},
                "tests": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["answer", "evidence_paths", "tests"],
            "additionalProperties": False,
        },
    },
}

def openrouter_direct_argv(response_format: str = "none") -> list[str]:
    """OpenRouter chat-completions lane: no agent CLI needed. The key comes
    from ~/.openrouter/api_key, the task text arrives as the script's $0 (safe
    for any quoting/newlines), and the request body is built with json.dumps —
    no hand-escaped JSON in shell. urllib retries transient 429/5xx failures
    so one rate-limit blip no longer blanks a lane."""
    script = "\n".join([
        "import json, os, ssl, sys, time, urllib.request, urllib.error",
        "try:",
        "    import certifi",
        "    ssl_context = ssl.create_default_context(cafile=certifi.where())",
        "except ImportError:",
        "    ssl_context = ssl.create_default_context()",
        'model = os.environ["LEIO_BENCH_MODEL"]',
        "prompt = sys.argv[1]",
        'key = open(os.path.expanduser("~/.openrouter/api_key")).read().strip()',
        'body = {"model": model, "messages": [{"role": "user", "content": prompt}]}',
        'rf = os.environ.get("LEIO_BENCH_RF")',
        "if rf:",
        "    body['response_format'] = json.loads(rf)",
        'data = json.dumps(body).encode()',
        'last_error = None',
        "for attempt in range(3):",
        "    try:",
        '        req = urllib.request.Request(',
        '            "https://openrouter.ai/api/v1/chat/completions",',
        "            data=data,",
        '            headers={"Authorization": "Bearer " + key,',
        '                     "Content-Type": "application/json"},',
        "        )",
        "        print(urllib.request.urlopen(req, timeout=120, context=ssl_context).read().decode())",
        "        last_error = None",
        "        break",
        "    except urllib.error.HTTPError as error:",
        "        last_error = error",
        "        if error.code not in (429, 500, 502, 503, 529):",
        "            print(error.read().decode(errors='replace'))",
        "            break",
        "        time.sleep(5 * (attempt + 1))",
        "    except Exception as error:",
        "        last_error = error",
        "        time.sleep(5 * (attempt + 1))",
        "if last_error is not None:",
        "    raise SystemExit(f'lane model call failed: {last_error}')",
    ])
    return ["python3", "-c", script, "{task}"]

DEFAULT_TEMPLATES = {
    "codex-model": {"argv": ["codex", "exec", "--model", "{model}", "{task}"]},
    "claude-model": {"argv": ["claude", "-p", "--model", "{model}", "{task}"]},
    # Direct OpenRouter chat-completions lane: no agent CLI needed, the key
    # comes from ~/.openrouter/api_key, and the task text arrives as the
    # script's $0 (safe for any quoting/newlines).
    "openrouter-direct": {"argv": openrouter_direct_argv("none")},}

MAX_CONTEXT_CHARS = 60_000

VERDICT_SCHEMA_EXAMPLE = (
    '{"answer": "2-4 sentences", '
    '"evidence_paths": ["repo/relative/path", ...], '
    '"tests": ["command or test name", ...]}'
)

# Named prompt treatments for the small-model gap investigation. Each
# builder returns the lane instruction given the base task text; the runner
# appends grounding sections after the instruction.
def _base_instruction(task: str) -> str:
    return (
        "Work inside this repository. Ground every claim in the LEIO code view "
        "and the governed sources provided below — LEIO evidence and Reference Provider "
        "context are MANDATORY inputs, not optional references: "
        "run `leio-code index` if the code view is stale, then "
        "`leio-code context --repo . '{task}' --json` and, where useful, "
        "`leio-code find` / `leio-code graph`. "
        "Finish by printing EXACTLY ONE JSON object (no code fences, no prose "
        "after it) with this shape:\n"
        + VERDICT_SCHEMA_EXAMPLE
        + "\n`evidence_paths` must cite the repository files that ground the "
        "answer; `tests` must list how the change would be verified. "
        "The task: " + task
    )

def _schema_last(task: str) -> str:
    return _base_instruction(task) + (
        "\n\nOUTPUT CONTRACT (overrides anything above): after reading the "
        "grounding sections, your ENTIRE final message must be exactly one JSON "
        "object and nothing else — no prose, no code fences:\n"
        + VERDICT_SCHEMA_EXAMPLE
        + "\nFill it from the evidence you read. Every path in evidence_paths "
        "must appear verbatim in the grounding sections."
    )

def _citation_checklist(task: str) -> str:
    return _base_instruction(task) + (
        "\n\nEVIDENCE REQUIREMENTS: evidence_paths MUST contain at least 3 "
        "entries drawn verbatim from the grounding sections, including at "
        "least one instruction/memory document (e.g. GEMINI.md, AGENTS.md, "
        "docs/agent-memory.md) and at least one code file. `tests` MUST list "
        "the verification commands named in the grounding sections. Then "
        "output exactly one JSON object:\n" + VERDICT_SCHEMA_EXAMPLE
    )

def _category_scan(task: str) -> str:
    return _base_instruction(task) + (
        "\n\nPROCEDURE: before answering, scan the grounding sections zone "
        "by zone — instructions, memory, anchors, ranked files, verification, "
        "risks — and pick the strongest evidence from each. Cite at least one "
        "instruction/memory path AND one code path in evidence_paths. Your "
        "final message must be exactly one JSON object:\n"
        + VERDICT_SCHEMA_EXAMPLE
    )

def _schema_last_checklist(task: str) -> str:
    return _citation_checklist(task) + (
        "\n\nREMINDER: your ENTIRE final message is the JSON object alone — "
        "no surrounding text. Re-check that every evidence path appears "
        "verbatim in the grounding sections before you finish."
    )

PROMPT_VARIANTS = {
    "baseline": _base_instruction,
    "schema-last": _schema_last,
    "citation-checklist": _citation_checklist,
    "category-scan": _category_scan,
    "schema-last+checklist": _schema_last_checklist,
}

VERDICT_INSTRUCTION = (
    "Work inside this repository. Ground every claim in the LEIO code view "
    "and the governed sources provided below — LEIO evidence and Reference Provider "
    "context are MANDATORY inputs, not optional references: "
    "run `leio-code index` if the code view is stale, then "
    "`leio-code context --repo . '{task}' --json` and, where useful, "
    "`leio-code find` / `leio-code graph`. "
    "Finish by printing EXACTLY ONE JSON object (no code fences, no prose "
    "after it) with this shape:\n"
    '{{"answer": "2-4 sentences", "evidence_paths": ["repo/relative/path", ...], '
    '"tests": ["command or test name", ...]}}\n'
    "`evidence_paths` must cite the repository files that ground the answer; "
    "`tests` must list how the change would be verified."
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Model-swept LEIO golden-task benchmark")
    parser.add_argument("--models", nargs="+", default=DEFAULT_MODELS, help="OpenRouter model slugs")
    parser.add_argument(
        "--agent",
        default="codex-model",
        help="Agent template key whose argv carries {model} (default: codex-model)",
    )
    parser.add_argument(
        "--argv",
        nargs="+",
        help="Custom template argv with {model}/{task}; overrides --agent",
    )
    parser.add_argument("--tasks", type=Path, default=PLUGIN_ROOT / "benchmarks" / "context-golden-tasks.json")
    parser.add_argument("--repo", type=Path, default=PLUGIN_ROOT, help="Repo the lanes work in")
    parser.add_argument("--worktree-root", type=Path, default=Path("/tmp/leio-bench-wt"))
    parser.add_argument("--out", type=Path, default=PLUGIN_ROOT / "benchmarks" / "model-sweep")
    parser.add_argument("--timeout-ms", type=int, default=600_000)
    parser.add_argument("--parallel", action="store_true")
    parser.add_argument("--bus", default=None, help="Arrow Flight bus addr, e.g. 127.0.0.1:18815")
    parser.add_argument("--harness", default="leio-harness", help="leio-harness binary")
    parser.add_argument("--leio-bin", default="", help="leio-code binary used for --with-context bundles")
    parser.add_argument(
        "--response-format", default="json_schema", choices=["none", "json_object", "json_schema"],
        help="OpenRouter response_format for openrouter-direct lanes. Default "
        "json_schema: measured on qwen3.8-flash it removes the last format "
        "slip (4/5 -> 5/5 json_valid) by enforcing the verdict shape; pass "
        "none for models that reject structured outputs")
    parser.add_argument(
        "--prompt-variant", default="category-scan", choices=sorted(PROMPT_VARIANTS),
        help="Prompt treatment. Default category-scan: measured on qwen3.8-flash "
        "it lifts code-task evidence 0.089 -> 0.261 (past claude-fable-5.1's "
        "0.197 baseline) and matches fable's reference reflection at 6/6 JSON; "
        "pass baseline to reproduce pre-variant numbers")
    parser.add_argument("--execute", action="store_true", help="Run the day (spends model tokens)")
    parser.add_argument(
        "--with-context",
        action="store_true",
        help="Embed each task's LEIO context bundle into the lane prompt "
        "(required for openrouter-direct lanes, which cannot run tools)",
    )
    parser.add_argument(
        "--reference-answers", type=Path, default=None,
        help="Optional local JSON object mapping task names to reference notes; no external service is started",
    )
    return parser.parse_args()


def expected_any(task: dict) -> list[str]:
    out: list[str] = []
    for key in (
        "expected_paths_any",
        "expected_instruction_paths_any",
        "expected_memory_paths_any",
        "expected_tests_any",
        "expected_evidence_any",
    ):
        out.extend(task.get(key) or [])
    return out


def short_model(slug: str) -> str:
    tail = slug.split("/")[-1]
    return re.sub(r"[^a-z0-9]+", "-", tail.lower()).strip("-")[:32]


def build_spec(args: argparse.Namespace, tasks: list[dict]) -> tuple[dict, list[dict]]:
    templates = dict(DEFAULT_TEMPLATES)
    agent_key = args.agent
    if args.argv:
        agent_key = "custom-model"
        templates[agent_key] = {"argv": args.argv}
    if agent_key == "openrouter-direct":
        templates["openrouter-direct"] = {
            "argv": openrouter_direct_argv(args.response_format),
            "env": {
                "LEIO_BENCH_MODEL": "{model}",
                **(
                    {"LEIO_BENCH_RF": json.dumps(
                        VERDICT_RESPONSE_FORMAT_SCHEMA, separators=(",", ":"))}
                    if args.response_format == "json_schema"
                    else (
                        {"LEIO_BENCH_RF": '{"type":"json_object"}'}
                        if args.response_format == "json_object" else {}
                    )
                ),
            },
        }
    if agent_key not in templates:
        raise SystemExit(f"unknown agent template: {agent_key}")

    reference_answers: dict[str, str] = {}
    reference_path = getattr(args, "reference_answers", None)
    if reference_path is not None:
        reference_answers = json.loads(Path(reference_path).read_text())
        if not isinstance(reference_answers, dict) or not all(
            isinstance(key, str) and isinstance(value, str)
            for key, value in reference_answers.items()
        ):
            raise ValueError("reference answers must map task names to text")

    bundles: dict[str, str] = {}
    if getattr(args, "with_context", False):
        leio = args.leio_bin or "leio-code"
        for task in tasks:
            proc = subprocess.run(
                [leio, "context", "--repo", str(args.repo.resolve()), task["task"], "--json"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=120,
            )
            bundle = proc.stdout.strip()
            if len(bundle) > MAX_CONTEXT_CHARS:
                bundle = bundle[:MAX_CONTEXT_CHARS] + "\n... (truncated)"
            bundles[task["name"]] = bundle or "{}"

    lanes: list[dict] = []
    plan: list[dict] = []
    for task in tasks:
        for model in args.models:
            agent_id = f"bench-{short_model(model)}-{short_model(task['name'])}"
            instruction = PROMPT_VARIANTS[args.prompt_variant](task["task"])
            lane_task = instruction
            if args.with_context:
                lane_task += (
                    "\n\nLEIO CONTEXT BUNDLE (authoritative evidence for this "
                    f"task; cite from here):\n{bundles[task['name']]}"
                )
            if reference_answers:
                lane_task += (
                    "\n\nREFERENCE NOTES (user-supplied evidence — inspect its "
                    "claims before citing it when it bears on the task):\n"
                    + (reference_answers.get(task["name"]) or "{}")
                )
            lanes.append(
                {
                    "agentId": agent_id,
                    "agent": agent_key,
                    "model": model,
                    "task": lane_task,
                    "timeoutMs": args.timeout_ms,
                }
            )
            plan.append({"agentId": agent_id, "model": model, "task": task})

    goal = "Model sweep: LEIO golden context tasks x " + ", ".join(args.models)
    spec = {
        "goal": goal,
        "repo": str(args.repo.resolve()),
        "worktreeRoot": str(args.worktree_root),
        "outputDir": str(args.out.resolve()),
        "timeoutMs": args.timeout_ms,
        "parallel": args.parallel,
        "requireFreshCodeView": True,
        "agentTemplates": templates,
        "lanes": lanes,
    }
    return spec, plan, reference_answers


def extract_last_json(text: str) -> dict | None:
    decoder = json.JSONDecoder()
    best: tuple[dict, int] | None = None
    for match in re.finditer(r"\{", text):
        try:
            obj, end = decoder.raw_decode(text[match.start():])
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            absolute_end = match.start() + end
            if best is None or absolute_end > best[1]:
                best = (obj, absolute_end)
    return best[0] if best else None


def grade_lane(outcome: dict, task: dict, output_dir: Path, reference_text: str | None = None) -> dict:
    grade = {
        "agentId": outcome.get("agentId"),
        "status": outcome.get("status"),
        "duration_ms": outcome.get("durationMs"),
        "json_valid": False,
        "evidence_ratio": 0.0,
        "evidence_hits": [],
        "tests_hit": [],
        "error": outcome.get("error"),
    }
    stdout_path = None
    for artifact in outcome.get("artifacts") or []:
        p = artifact.get("path", "") if isinstance(artifact, dict) else str(artifact)
        if f"lane:{outcome.get('agentId')}" == artifact.get("producer") and p.endswith("stdout.log"):
            stdout_path = Path(p)
            break
        if p.endswith("stdout.log") and outcome.get("agentId") in p:
            stdout_path = Path(p)
    if stdout_path is None:
        for artifact in outcome.get("artifacts") or []:
            p = artifact.get("path", "") if isinstance(artifact, dict) else str(artifact)
            if p.endswith("stdout.log"):
                stdout_path = Path(p)
                break
    if stdout_path is None or not stdout_path.exists():
        grade["error"] = grade["error"] or "stdout artifact not found"
        return grade

    verdict = extract_last_json(stdout_path.read_text(errors="replace"))
    # OpenAI-compatible API responses (openrouter-direct lanes) wrap the
    # verdict inside choices[0].message.content — unwrap before grading.
    for _ in range(3):
        if isinstance(verdict, dict) and "choices" in verdict:
            content = ((verdict.get("choices") or [{}])[0].get("message") or {}).get("content")
            verdict = extract_last_json(content or "")
        elif isinstance(verdict, dict) and "answer" not in verdict and "content" in verdict:
            verdict = extract_last_json(str(verdict.get("content") or ""))
        else:
            break
    if not isinstance(verdict, dict) or "answer" not in verdict:
        grade["error"] = grade["error"] or "no parseable JSON verdict in lane stdout"
        return grade

    grade["json_valid"] = True
    verdict_text = (
        str(verdict.get("answer", ""))
        + " "
        + " ".join(str(p) for p in (verdict.get("evidence_paths") or []))
        + " "
        + " ".join(str(t) for t in (verdict.get("tests") or []))
    )
    if reference_text:
        markers = []
        for token in re.findall(
            r"\b[A-Z][A-Za-z0-9-]{3,}\b|\b(?:signed|unsigned|governed|refusal|forecast|ROAS|sell-through|not-governed)\b",
            reference_text,
        ):
            if token.lower() not in {m.lower() for m in markers} and len(markers) < 12:
                markers.append(token)
        lowered = verdict_text.lower()
        hits = [m for m in markers if m.lower() in lowered]
        grade["reference_markers"] = markers
        grade["reference_hits"] = hits
        grade["reference_reflected"] = bool(hits)
        grade["reference_score"] = round(len(hits) / len(markers), 3) if markers else None
    else:
        grade["reference_reflected"] = None
    cited = {str(p).lstrip("./") for p in (verdict.get("evidence_paths") or [])}
    targets = {str(p).lstrip("./") for p in expected_any(task)}
    def hits(side_a: set[str], side_b: set[str]) -> list[str]:
        matched = []
        for want in side_b:
            for got in side_a:
                if want in got or got in want:
                    matched.append(want)
                    break
        return matched

    evidence_matched = hits(cited, targets)
    grade["evidence_hits"] = evidence_matched
    grade["evidence_ratio"] = round(len(evidence_matched) / len(targets), 3) if targets else 0.0
    cited_blob = (
        " ".join(cited)
        + " "
        + str(verdict.get("answer", ""))
        + " "
        + " ".join(str(t) for t in (verdict.get("tests") or []))
    )
    grade["tests_hit"] = [t for t in (task.get("expected_tests_any") or []) if t in cited_blob]
    return grade


def main() -> int:
    args = parse_args()
    fixture = json.loads(args.tasks.read_text())
    tasks = fixture["tasks"]
    spec, plan, reference_answers = build_spec(args, tasks)

    args.out.mkdir(parents=True, exist_ok=True)
    spec_path = args.out / "day-spec.json"
    spec_path.write_text(json.dumps(spec, indent=2) + "\n")
    print(f"day spec: {spec_path} ({len(plan)} lanes)")

    if not args.execute:
        print(f"dry-run: execute with `{args.harness} day --spec {spec_path}`")
        return 0

    cmd = [args.harness, "day", "--spec", str(spec_path)]
    if args.bus:
        cmd += ["--bus", args.bus]
    started = time.time()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if proc.returncode != 0:
        print(proc.stderr[-4000:], file=sys.stderr)
        raise SystemExit(f"leio-harness day failed with code {proc.returncode}")
    print(f"day wall clock: {time.time() - started:.1f}s")

    report = json.loads(proc.stdout[proc.stdout.index("{"):])
    task_by_agent = {p["agentId"]: p["task"] for p in plan}
    outcomes_by_agent = {o.get("agentId"): o for o in report.get("outcomes", [])}
    grades = []
    for entry in plan:
        agent_id = entry["agentId"]
        outcome = outcomes_by_agent.get(agent_id)
        if outcome is None:
            grades.append({
                "agentId": agent_id,
                "model": entry["model"],
                "status": "missing",
                "error": "no outcome in day report",
            })
            continue
        reference_for_lane = reference_answers.get(entry["task"]["name"])
        grade = grade_lane(outcome, task_by_agent[agent_id], args.out, reference_text=reference_for_lane)
        grade["model"] = entry["model"]
        grades.append(grade)
    for agent_id, outcome in outcomes_by_agent.items():
        if agent_id not in task_by_agent:
            grades.append({
                "agentId": agent_id,
                "status": outcome.get("status"),
                "error": "no plan entry",
            })

    by_model: dict[str, dict] = {}
    for grade in grades:
        if "model" not in grade:
            continue  # outcomes with no plan entry are reported verbatim in lanes
        bucket = by_model.setdefault(
            grade["model"],
            {"lanes": 0, "graded": 0, "missing": 0, "json_valid": 0, "evidence": [], "durations": [], "reference": []},
        )
        bucket["lanes"] += 1
        if grade.get("status") == "missing" or grade.get("error") == "no plan entry":
            bucket["missing"] += 1
            continue
        bucket["graded"] += 1
        bucket["json_valid"] += int(grade.get("json_valid", False))
        bucket["evidence"].append(grade.get("evidence_ratio") or 0.0)
        if grade.get("reference_reflected") is not None:
            bucket["reference"].append(int(grade["reference_reflected"]))
        if grade.get("status") == "passed":
            bucket["durations"].append(grade.get("duration_ms") or 0)

    summary = {
        "goal": spec["goal"],
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "models": [
            {
                "model": model,
                "lanes": b["lanes"],
                "graded": b["graded"],
                "missing": b["missing"],
                "json_valid": b["json_valid"],
                "mean_evidence_ratio": round(sum(b["evidence"]) / len(b["evidence"]), 3) if b["evidence"] else 0.0,
                "reference_reflected_rate": round(sum(b["reference"]) / len(b["reference"]), 3) if b["reference"] else None,
                "median_duration_ms": sorted(b["durations"])[len(b["durations"]) // 2] if b["durations"] else None,
            }
            for model, b in by_model.items()
        ],
        "lanes": grades,
    }
    report_json = args.out / "model-sweep-report.json"
    report_json.write_text(json.dumps(summary, indent=2) + "\n")

    lines = ["# LEIO model sweep (golden tasks)", "", "| model | lanes | json_valid | mean evidence | median ms |", "| --- | ---: | ---: | ---: | ---: |"]
    for m in summary["models"]:
        lines.append(
            f"| {m['model']} | {m['lanes']} | {m['json_valid']} | {m['mean_evidence_ratio']} | {m['median_duration_ms']} |"
        )
    (args.out / "model-sweep-report.md").write_text("\n".join(lines) + "\n")

    print(json.dumps(summary["models"], indent=2))
    print("\n```widget")
    print(json.dumps({
        "title": "LEIO model sweep",
        "body": f"{len(plan)} lanes finished",
        "actions": [{
            "label": "Re-run sweep",
            "argv": ["bash", str(Path.home() / "projects" / "leio-workbench" / "scripts" / "jai-bench"), "--execute"],
        }],
    }))
    print("```")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
