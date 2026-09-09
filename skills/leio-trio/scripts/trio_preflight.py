#!/usr/bin/env python3
"""Trio preflight — deterministic gates before Ground and Execute.

Usage:
    trio_preflight.py /abs/repo [/abs/repo2 ...] [--tier a|b] [--quiet] [--no-harness]

--no-harness: the user forbade harness commands for this task; run only the
LEIO/git checks and report the harness-backed ones as unchecked.

Checks (names only, never secret values):
  * leio-code / leio-harness binaries and versions
  * per repo: exists, absolute, git dirty count, LEIO index presence and age,
    doctor summary line from `leio-code status`, code-view freshness from
    `leio-harness codeview check`
  * model table source (`leio-harness models show`)
  * embedding configuration presence (env or ~/.config/leio-harness/env)
  * which harness verbs the INSTALLED binary exposes (day, codeview, integrate, gate, …)
  * presence of an reference-provider MCP server entry (names only)

Output: one JSON object on stdout. Human summary on stderr unless --quiet.
Exit codes: 0 = no blocking gate for the requested tier, 1 = blocking gate,
2 = usage error. Tier B (harness day) additionally blocks on dirty trees and
stale code views; Tier A only warns about them.

Reference reachability is not checked here: it is an MCP call the host makes.
Stdlib only.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time

HOME = os.path.expanduser("~")
EMBED_KEYS = ("LEIO_HARNESS_EMBED_URL", "LEIO_HARNESS_EMBED_MODEL")
EMBED_ENV_FILE = os.path.join(HOME, ".config", "leio-harness", "env")


def resolve_binary(env_name: str, name: str) -> str | None:
    candidate = os.environ.get(env_name) or os.path.join(HOME, ".cargo", "bin", name)
    if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
        return candidate
    return shutil.which(name)


def run(argv: list[str], cwd: str | None = None, timeout: int = 180) -> tuple[int, str, str]:
    try:
        proc = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return proc.returncode, proc.stdout, proc.stderr
    except FileNotFoundError:
        return 127, "", f"not found: {argv[0]}"
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout}s: {' '.join(argv)}"


def version_of(binary: str | None) -> str | None:
    if not binary:
        return None
    code, out, err = run([binary, "--version"], timeout=30)
    text = (out or err).strip()
    return text if code == 0 and text else None


def git_dirty_count(repo: str) -> int | None:
    code, out, _ = run(["git", "-C", repo, "status", "--porcelain"], timeout=60)
    if code != 0:
        return None
    return len([line for line in out.splitlines() if line.strip()])


def leio_status(binary: str, repo: str) -> dict:
    result: dict = {"ok": False, "index_line": None, "doctor_line": None, "raw_error": None}
    code, out, err = run([binary, "--json", "--repo", repo, "status"], cwd=repo)
    if code != 0:
        result["raw_error"] = (err or out).strip()[:400]
        return result
    try:
        payload = json.loads(out)
    except json.JSONDecodeError:
        result["raw_error"] = "status did not return JSON"
        return result
    result["ok"] = bool(payload.get("ok", True))
    for line in payload.get("sections", []) or []:
        if isinstance(line, str) and line.startswith("Index:"):
            result["index_line"] = line
        elif isinstance(line, str) and line.startswith("Doctors:"):
            result["doctor_line"] = line
    caps = payload.get("workspace_capabilities") or {}
    result["workspace_profile"] = caps.get("workspace_profile")
    return result


def index_age_hours(repo: str) -> float | None:
    path = os.path.join(repo, ".leio-code", "index.json")
    if not os.path.isfile(path):
        return None
    return round((time.time() - os.path.getmtime(path)) / 3600.0, 2)


def codeview_check(binary: str | None, repo: str) -> dict:
    if not binary:
        return {"checked": False, "fresh": None, "detail": "leio-harness missing"}
    code, out, err = run([binary, "codeview", "check", "--repo", repo], cwd=repo)
    text = (out + "\n" + err).strip()
    fresh: bool | None
    lowered = text.lower()
    if code == 0 and "stale" not in lowered:
        fresh = True
    elif "stale" in lowered or code != 0:
        fresh = False
    else:
        fresh = None
    return {"checked": True, "fresh": fresh, "exit_code": code, "detail": text[:300]}


def models_show(binary: str | None) -> dict:
    if not binary:
        return {"available": False}
    code, out, err = run([binary, "models", "show"], timeout=60)
    if code != 0:
        return {"available": False, "detail": (err or out).strip()[:200]}
    try:
        payload = json.loads(out)
    except json.JSONDecodeError:
        return {"available": True, "source": None, "detail": out.strip()[:200]}
    return {
        "available": True,
        "source": payload.get("source"),
        "volume": payload.get("volume"),
        "verifier": payload.get("verifier"),
        "quality": payload.get("quality"),
    }


REQUIRED_HARNESS_VERBS = ("day", "codeview", "integrate", "gate", "models", "bus", "worktree", "lease")


def harness_verbs(binary: str | None) -> dict:
    """Which verbs the INSTALLED harness actually exposes — the checkout may be ahead of it."""
    if not binary:
        return {"checked": False, "present": [], "missing": list(REQUIRED_HARNESS_VERBS)}
    code, out, err = run([binary, "--help"], timeout=30)
    text = out + "\n" + err
    present = [v for v in REQUIRED_HARNESS_VERBS if re.search(rf"^\s+{re.escape(v)}\b", text, re.M)]
    extra = [v for v in ("shm", "compare", "merge", "sigreg", "gepa", "embed", "agent") if re.search(rf"^\s+{re.escape(v)}\b", text, re.M)]
    return {"checked": code == 0, "present": present, "missing": [v for v in REQUIRED_HARNESS_VERBS if v not in present], "also": extra}


def reference_mcp_configured(repos: list[str]) -> dict:
    """Presence of an reference-provider MCP server entry (names only; reachability is host-checked)."""
    candidates = [os.path.join(HOME, ".claude.json")] + [os.path.join(r, ".mcp.json") for r in repos]
    found: list[str] = []
    for path in candidates:
        if not os.path.isfile(path):
            continue
        try:
            with open(path, "r", encoding="utf-8") as handle:
                if "reference-provider" in handle.read():
                    found.append(path)
        except OSError:
            continue
    return {"configured_in": found, "configured": bool(found)}


def embeddings_configured() -> dict:
    present = {k: bool(os.environ.get(k)) for k in EMBED_KEYS}
    file_keys: set[str] = set()
    if os.path.isfile(EMBED_ENV_FILE):
        with open(EMBED_ENV_FILE, "r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                key = line.split("=", 1)[0].strip()
                if key.startswith("LEIO_HARNESS_EMBED_"):
                    file_keys.add(key)
    configured = all(present[k] or k in file_keys for k in EMBED_KEYS)
    return {
        "configured": configured,
        "from_env": [k for k, v in present.items() if v],
        "from_file": sorted(file_keys),
        "file": EMBED_ENV_FILE if os.path.isfile(EMBED_ENV_FILE) else None,
    }


def main(argv: list[str]) -> int:
    repos: list[str] = []
    tier = "a"
    quiet = False
    no_harness = False
    it = iter(argv)
    for arg in it:
        if arg == "--tier":
            tier = next(it, "a").lower()
        elif arg == "--quiet":
            quiet = True
        elif arg == "--no-harness":
            # the user forbade harness commands: skip every leio-harness invocation
            # (codeview check, models show, --help/--version) and say so in the output
            no_harness = True
        elif arg.startswith("-"):
            print(__doc__, file=sys.stderr)
            return 2
        else:
            repos.append(arg)
    if not repos or tier not in ("a", "b"):
        print(__doc__, file=sys.stderr)
        return 2

    blocking: list[str] = []
    tier_b_blocking: list[str] = []
    warnings: list[str] = []

    leio_bin = resolve_binary("LEIO_CODE_BIN", "leio-code")
    harness_bin = resolve_binary("LEIO_HARNESS_BIN", "leio-harness")
    binaries = {
        "leio-code": {"path": leio_bin, "version": version_of(leio_bin)},
        "leio-harness": {"path": harness_bin, "version": None if no_harness else version_of(harness_bin), "invoked": not no_harness},
    }
    if no_harness:
        harness_bin = None  # every harness-backed check below degrades to "not checked"
        warnings.append("--no-harness: leio-harness was not invoked (user forbade harness commands); code-view freshness, model table and verbs are unchecked")
    if not leio_bin:
        blocking.append("leio-code binary not found (LEIO_CODE_BIN, ~/.cargo/bin, PATH)")
    if not harness_bin and not no_harness:
        (tier_b_blocking if tier == "b" else warnings).append(
            "leio-harness binary not found; Tier B and codeview checks unavailable"
        )

    repo_reports = []
    for repo in repos:
        report: dict = {"repo_root": repo}
        if not os.path.isabs(repo):
            blocking.append(f"{repo}: repo_root must be absolute")
            repo_reports.append(report)
            continue
        if not os.path.isdir(repo):
            blocking.append(f"{repo}: not a directory")
            repo_reports.append(report)
            continue
        dirty = git_dirty_count(repo)
        report["git_dirty_entries"] = dirty
        if dirty is None:
            warnings.append(f"{repo}: not a git checkout (harness worktrees impossible)")
            if tier == "b":
                tier_b_blocking.append(f"{repo}: not a git checkout")
        elif dirty > 0:
            msg = f"{repo}: {dirty} uncommitted entries — harness refuses worktrees on a dirty tree"
            (tier_b_blocking if tier == "b" else warnings).append(msg)

        age = index_age_hours(repo)
        report["index_age_hours"] = age
        if age is None:
            blocking.append(f"{repo}: no .leio-code/index.json — run `leio-code index --repo {repo}`")
        elif age > 24:
            warnings.append(f"{repo}: index is {age}h old — consider `leio-code index`")

        if leio_bin:
            status = leio_status(leio_bin, repo)
            report["leio_status"] = status
            if not status["ok"]:
                blocking.append(f"{repo}: leio-code status failed: {status.get('raw_error')}")
            doctor_line = status.get("doctor_line") or ""
            match = re.search(r"(\d+)/(\d+) with warnings", doctor_line)
            if match and int(match.group(1)) > 0:
                warnings.append(f"{repo}: doctor baseline already carries warnings — {doctor_line}")

        view = codeview_check(harness_bin, repo)
        report["code_view"] = view
        if view.get("checked") and view.get("fresh") is False:
            msg = f"{repo}: code view stale — run `leio-code index --repo {repo}` then re-check"
            (tier_b_blocking if tier == "b" else warnings).append(msg)
        repo_reports.append(report)

    models = models_show(harness_bin)
    if models.get("available") and models.get("source") not in (None, "openrouter-api"):
        warnings.append(
            f"model table source is {models.get('source')} — run `leio-harness models refresh` before a Tier B day"
        )

    embeddings = embeddings_configured()
    if not embeddings["configured"]:
        warnings.append(
            "embedding config missing (LEIO_HARNESS_EMBED_URL/_MODEL) — bus falls back to one-hot vectors; merge gate loses semantics"
        )

    verbs = harness_verbs(harness_bin)
    if verbs["checked"] and verbs["missing"]:
        msg = f"installed leio-harness lacks verb(s) {verbs['missing']} — the checkout may be ahead of the binary; rebuild before promising them"
        (tier_b_blocking if tier == "b" and any(v in verbs["missing"] for v in ("day", "codeview", "integrate", "gate")) else warnings).append(msg)

    reference = reference_mcp_configured([r for r in repos if os.path.isdir(r)])
    if not reference["configured"]:
        warnings.append("no reference-provider MCP server entry found in ~/.claude.json or <repo>/.mcp.json — Govern will have no governed surface")

    ok = not blocking and not (tier == "b" and tier_b_blocking)
    payload = {
        "ok": ok,
        "tier": tier,
        "blocking": blocking,
        "tier_b_blocking": tier_b_blocking,
        "warnings": warnings,
        "binaries": binaries,
        "harness_verbs": verbs,
        "repos": repo_reports,
        "models": models,
        "embeddings": embeddings,
        "reference": {**reference, "reachability": "host-checked: run one reference_provider_ask and record runtime.profile"},
    }
    print(json.dumps(payload, indent=2))

    if not quiet:
        status_word = "OK" if ok else "BLOCKED"
        print(f"trio preflight [{status_word}] tier={tier}", file=sys.stderr)
        for line in blocking:
            print(f"  BLOCK   {line}", file=sys.stderr)
        for line in tier_b_blocking:
            print(f"  TIER-B  {line}", file=sys.stderr)
        for line in warnings:
            print(f"  WARN    {line}", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
