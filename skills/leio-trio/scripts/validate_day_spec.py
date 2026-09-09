#!/usr/bin/env python3
"""Validate a leio-harness day spec against the trio's rules before spending tokens.

Usage:
    validate_day_spec.py day.json [--max-lanes 4]

Errors (exit 1): missing required fields, relative paths, unknown agent
template, unknown work shape, duplicate agentId, more than --max-lanes lanes
(design limit per repository), permission-bypass flags in argv, secret-looking
values in the spec, malformed integrate/objective.

Warnings (exit 0): requireFreshCodeView not true, workShape/model on a template
that never consumes {model} (built-ins ignore it), unknown top-level keys,
repo path missing or not a git checkout.

Output: one JSON object on stdout plus a human summary on stderr. Stdlib only.
"""
from __future__ import annotations

import json
import os
import re
import sys

BUILTIN_TEMPLATES: dict[str, list[str]] = {
    "codex": ["codex", "exec", "{task}"],
    "claude": ["claude", "-p", "{task}"],
    "gemini": ["gemini", "-p", "{task}"],
    "kimi": ["kimi", "-p", "{task}"],
    "grok": ["grok", "-p", "{task}"],
}
WORK_SHAPES = {
    "explorer": "explorer", "terra-low": "explorer",
    "worker": "worker", "terra-medium": "worker",
    "verifier": "verifier", "luna-medium": "verifier",
    "reviewer": "reviewer", "sol-high": "reviewer",
    "security": "security", "sol-xhigh": "security",
}
KNOWN_TOP_LEVEL = {
    "goal", "repo", "worktreeRoot", "outputDir", "timeoutMs", "parallel",
    "requireFreshCodeView", "agentTemplates", "lanes", "integrate",
}
KNOWN_LANE_KEYS = {"agentId", "task", "argv", "agent", "model", "workShape", "timeoutMs"}
BYPASS_FLAGS = {
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "--full-auto",
    "--yolo",
    "--no-sandbox",
    "--approval-mode=never",
}
SECRET_KEY_RE = re.compile(r"(KEY|TOKEN|SECRET|PASSWORD|PASSWD)", re.IGNORECASE)
PLACEHOLDER_RE = re.compile(r"^\s*(\{[^}]*\}|\$\{[^}]*\}|\$[A-Z_]+)?\s*$")
SECRET_LITERAL_RE = re.compile(r"(sk-[A-Za-z0-9]{8,}|-----BEGIN [A-Z ]*PRIVATE KEY-----|AKIA[0-9A-Z]{12,})")


def is_abs(value) -> bool:
    return isinstance(value, str) and os.path.isabs(value)


def consumes_model(argv: list[str], env: dict) -> bool:
    return any("{model}" in part for part in argv) or any(
        isinstance(v, str) and "{model}" in v for v in env.values()
    )


def main(argv: list[str]) -> int:
    if not argv or argv[0].startswith("-"):
        print(__doc__, file=sys.stderr)
        return 2
    path = argv[0]
    max_lanes = 4
    rest = iter(argv[1:])
    for arg in rest:
        if arg == "--max-lanes":
            try:
                max_lanes = int(next(rest))
            except (StopIteration, ValueError):
                print(__doc__, file=sys.stderr)
                return 2
        else:
            print(__doc__, file=sys.stderr)
            return 2

    errors: list[str] = []
    warnings: list[str] = []

    try:
        with open(path, "r", encoding="utf-8") as handle:
            raw = handle.read()
        spec = json.loads(raw)
    except (OSError, json.JSONDecodeError) as exc:
        print(json.dumps({"ok": False, "errors": [f"cannot read spec: {exc}"], "warnings": []}))
        return 1
    if not isinstance(spec, dict):
        print(json.dumps({"ok": False, "errors": ["spec must be a JSON object"], "warnings": []}))
        return 1

    if SECRET_LITERAL_RE.search(raw):
        errors.append("secret-looking literal present in the spec (API key / private key)")

    for key in ("goal", "repo", "worktreeRoot", "outputDir", "lanes"):
        if key not in spec:
            errors.append(f"missing required field: {key}")
    for key in sorted(set(spec) - KNOWN_TOP_LEVEL):
        warnings.append(f"unknown top-level key ignored by the runtime: {key}")

    for key in ("repo", "worktreeRoot", "outputDir"):
        value = spec.get(key)
        if value is not None and not is_abs(value):
            errors.append(f"{key} must be an absolute path (got {value!r})")
    repo = spec.get("repo")
    if is_abs(repo):
        if not os.path.isdir(repo):
            warnings.append(f"repo does not exist on this machine: {repo}")
        elif not os.path.isdir(os.path.join(repo, ".git")) and not os.path.isfile(os.path.join(repo, ".git")):
            warnings.append(f"repo is not a git checkout (harness worktrees need one): {repo}")

    timeout = spec.get("timeoutMs")
    if timeout is not None and (not isinstance(timeout, int) or timeout <= 0):
        errors.append("timeoutMs must be a positive integer")
    if spec.get("requireFreshCodeView") is not True:
        warnings.append("requireFreshCodeView is not true — a stale code view will not fail the day")

    templates: dict[str, dict] = {}
    for name, argv_list in BUILTIN_TEMPLATES.items():
        templates[name] = {"argv": argv_list, "env": {}, "builtin": True}
    custom = spec.get("agentTemplates") or {}
    if not isinstance(custom, dict):
        errors.append("agentTemplates must be an object")
        custom = {}
    for name, template in custom.items():
        if not isinstance(template, dict) or not isinstance(template.get("argv"), list) or not template["argv"]:
            errors.append(f"agentTemplates.{name}: argv must be a non-empty list")
            continue
        if not all(isinstance(part, str) for part in template["argv"]):
            errors.append(f"agentTemplates.{name}: argv entries must be strings")
            continue
        env = template.get("env") or {}
        if not isinstance(env, dict):
            errors.append(f"agentTemplates.{name}: env must be an object")
            env = {}
        for env_key, env_value in env.items():
            if SECRET_KEY_RE.search(env_key) and isinstance(env_value, str) and not PLACEHOLDER_RE.match(env_value):
                errors.append(f"agentTemplates.{name}.env.{env_key}: secret-looking value; secrets stay operator-side")
        bypass = [part for part in template["argv"] if part in BYPASS_FLAGS]
        if bypass:
            errors.append(f"agentTemplates.{name}: permission-bypass flag(s) {bypass}")
        templates[name] = {"argv": template["argv"], "env": env, "builtin": False}

    lanes = spec.get("lanes")
    lane_reports = []
    if not isinstance(lanes, list) or not lanes:
        errors.append("lanes must be a non-empty list")
        lanes = []
    if len(lanes) > max_lanes:
        if spec.get("parallel"):
            errors.append(
                f"parallel=true with {len(lanes)} lanes exceeds the design limit of {max_lanes} active lanes per repository"
            )
        else:
            warnings.append(
                f"{len(lanes)} lanes run serially (parallel=false), so at most one is active — fine, but a parallel day would need ≤ {max_lanes}"
            )

    seen_ids: set[str] = set()
    for index, lane in enumerate(lanes):
        label = f"lanes[{index}]"
        if not isinstance(lane, dict):
            errors.append(f"{label}: must be an object")
            continue
        for key in sorted(set(lane) - KNOWN_LANE_KEYS):
            warnings.append(f"{label}: unknown key ignored by the runtime: {key}")
        agent_id = lane.get("agentId")
        if not isinstance(agent_id, str) or not agent_id.strip():
            errors.append(f"{label}: agentId is required")
        elif agent_id in seen_ids:
            errors.append(f"{label}: duplicate agentId {agent_id!r}")
        else:
            seen_ids.add(agent_id)
        if not isinstance(lane.get("task"), str) or not lane["task"].strip():
            errors.append(f"{label}: task is required")
        lane_timeout = lane.get("timeoutMs")
        if lane_timeout is not None and (not isinstance(lane_timeout, int) or lane_timeout <= 0):
            errors.append(f"{label}: timeoutMs must be a positive integer")

        explicit_argv = lane.get("argv") or []
        template_name = lane.get("agent")
        resolved_argv: list[str] = []
        resolved_env: dict = {}
        source = None
        if explicit_argv:
            if not all(isinstance(part, str) for part in explicit_argv):
                errors.append(f"{label}: argv entries must be strings")
            resolved_argv = [p for p in explicit_argv if isinstance(p, str)]
            source = "explicit argv"
        elif template_name:
            template = templates.get(template_name)
            if template is None:
                errors.append(f"{label}: unknown agent template {template_name!r} (builtins: {', '.join(BUILTIN_TEMPLATES)}; custom: {', '.join(custom) or 'none'})")
            else:
                resolved_argv = template["argv"]
                resolved_env = template["env"]
                source = f"template {template_name}" + (" (builtin)" if template["builtin"] else "")
        else:
            errors.append(f"{label}: needs either argv or agent")

        bypass = [part for part in resolved_argv if part in BYPASS_FLAGS]
        if bypass:
            errors.append(f"{label}: permission-bypass flag(s) {bypass}")

        shape = lane.get("workShape")
        shape_norm = None
        if shape is not None:
            shape_norm = WORK_SHAPES.get(str(shape).strip().lower())
            if shape_norm is None:
                errors.append(f"{label}: unknown workShape {shape!r} (allowed: {', '.join(sorted(set(WORK_SHAPES)))})")
        model = lane.get("model")
        if (shape is not None or model) and resolved_argv and not consumes_model(resolved_argv, resolved_env):
            warnings.append(
                f"{label}: workShape/model set but the resolved invocation never consumes {{model}} — the CLI keeps its own default"
            )
        lane_reports.append({
            "agentId": agent_id,
            "invocation": source,
            "workShape": shape_norm,
            "model": model,
            "consumesModel": consumes_model(resolved_argv, resolved_env) if resolved_argv else None,
        })

    integrate = spec.get("integrate")
    if integrate is not None:
        if not isinstance(integrate, dict):
            errors.append("integrate must be an object")
        else:
            if not isinstance(integrate.get("targetRef"), str) or not integrate["targetRef"].strip():
                errors.append("integrate.targetRef is required")
            objective = integrate.get("objective")
            if objective is not None:
                if not isinstance(objective, dict):
                    errors.append("integrate.objective must be an object")
                else:
                    obj_argv = objective.get("argv")
                    if not isinstance(obj_argv, list) or not obj_argv or not all(isinstance(p, str) for p in obj_argv):
                        errors.append("integrate.objective.argv must be a non-empty list of strings")
                    if not is_abs(objective.get("outputDir")):
                        errors.append("integrate.objective.outputDir must be an absolute path")
                    obj_timeout = objective.get("timeoutMs")
                    if not isinstance(obj_timeout, int) or obj_timeout <= 0:
                        errors.append("integrate.objective.timeoutMs must be a positive integer")
            else:
                warnings.append("integrate has no objective — integration will merge without an improvement verdict")

    if spec.get("parallel") and 1 < len(lanes) <= max_lanes:
        warnings.append("parallel=true: lanes share the repository lease space and the same code view; keep briefs disjoint")

    ok = not errors
    print(json.dumps({"ok": ok, "errors": errors, "warnings": warnings, "lanes": lane_reports}, indent=2))
    print(f"day spec {'OK' if ok else 'INVALID'}: {len(errors)} error(s), {len(warnings)} warning(s)", file=sys.stderr)
    for line in errors:
        print(f"  ERROR {line}", file=sys.stderr)
    for line in warnings:
        print(f"  WARN  {line}", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
