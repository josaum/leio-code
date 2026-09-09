#!/usr/bin/env python3
"""Snapshot and diff repository side effects around a lane, a subagent, or a whole task.

Usage:
    trio_sideeffects.py snapshot /abs/repo [/abs/repo2 ...] --out state.json
    trio_sideeffects.py diff --against state.json [--allow <glob> ...]

`snapshot` records, per repository: HEAD, `git status --porcelain` entries, `agents/*` branches,
`git worktree list`, and untracked files. `.leio-code/` sidecars are recorded but excluded from the
verdict — LEIO appends PROV events on every call, and that is expected, not a write to the repo.

`diff` re-snapshots the same repositories and reports what changed: new or modified tracked
entries, new untracked files (minus --allow globs), new `agents/*` branches, new worktrees, moved
HEAD. Verdict `clean` exits 0; anything else exits 1.

Take the snapshot from the supervising session, before the lane starts. A snapshot the lane takes
of itself is self-attestation. Stdlib only.
"""
from __future__ import annotations

import fnmatch
import json
import os
import subprocess
import sys
import time


def git(repo: str, *args: str) -> str:
    try:
        proc = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return proc.stdout if proc.returncode == 0 else ""


def snapshot_repo(repo: str) -> dict:
    porcelain = [line.rstrip("\n") for line in git(repo, "status", "--porcelain", "--untracked-files=all").splitlines() if line.strip()]
    tracked = sorted(line for line in porcelain if not line.startswith("??") and ".leio-code/" not in line)
    untracked = sorted(line[3:] for line in porcelain if line.startswith("??") and ".leio-code/" not in line)
    sidecar = sorted(line for line in porcelain if ".leio-code/" in line)
    branches = sorted(b.strip() for b in git(repo, "branch", "--list", "agents/*", "--format=%(refname:short)").splitlines() if b.strip())
    worktrees = sorted(line.split(" ", 1)[1] for line in git(repo, "worktree", "list", "--porcelain").splitlines() if line.startswith("worktree "))
    return {
        "repo": repo,
        "head": git(repo, "rev-parse", "HEAD").strip() or None,
        "tracked_changes": tracked,
        "untracked": untracked,
        "sidecar_entries": sidecar,
        "agent_branches": branches,
        "worktrees": worktrees,
    }


def take_snapshot(repos: list[str]) -> dict:
    return {"taken_at": int(time.time()), "repos": [snapshot_repo(r) for r in repos]}


def diff_snapshots(before: dict, allow: list[str]) -> dict:
    report = {"verdict": "clean", "repos": []}
    for prev in before.get("repos", []):
        repo = prev["repo"]
        now = snapshot_repo(repo)
        new_tracked = sorted(set(now["tracked_changes"]) - set(prev["tracked_changes"]))
        new_untracked = sorted(
            p for p in set(now["untracked"]) - set(prev["untracked"])
            if not any(fnmatch.fnmatch(p, g) for g in allow)
        )
        entry = {
            "repo": repo,
            "head_moved": prev["head"] != now["head"],
            "head_before": prev["head"], "head_after": now["head"],
            "new_tracked_changes": new_tracked,
            "new_untracked": new_untracked,
            "new_agent_branches": sorted(set(now["agent_branches"]) - set(prev["agent_branches"])),
            "new_worktrees": sorted(set(now["worktrees"]) - set(prev["worktrees"])),
            "sidecar_changed": now["sidecar_entries"] != prev["sidecar_entries"],
        }
        if entry["head_moved"] or new_tracked or new_untracked or entry["new_agent_branches"] or entry["new_worktrees"]:
            report["verdict"] = "dirty"
        report["repos"].append(entry)
    return report


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    mode, rest = argv[0], argv[1:]
    if mode == "snapshot":
        repos, out = [], None
        it = iter(rest)
        for arg in it:
            if arg == "--out":
                out = next(it, None)
            elif arg.startswith("-"):
                print(__doc__, file=sys.stderr)
                return 2
            else:
                repos.append(arg)
        if not repos or not out or not all(os.path.isabs(r) for r in repos):
            print(__doc__, file=sys.stderr)
            return 2
        snap = take_snapshot(repos)
        os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
        json.dump(snap, open(out, "w", encoding="utf-8"), indent=2)
        print(json.dumps({"ok": True, "out": out, "repos": [r["repo"] for r in snap["repos"]]}))
        return 0
    if mode == "diff":
        against, allow = None, []
        it = iter(rest)
        for arg in it:
            if arg == "--against":
                against = next(it, None)
            elif arg == "--allow":
                allow.append(next(it, ""))
            else:
                print(__doc__, file=sys.stderr)
                return 2
        if not against or not os.path.isfile(against):
            print(__doc__, file=sys.stderr)
            return 2
        before = json.load(open(against, "r", encoding="utf-8"))
        report = diff_snapshots(before, allow)
        print(json.dumps(report, indent=2))
        print(f"trio side effects: {report['verdict']}", file=sys.stderr)
        for entry in report["repos"]:
            for key in ("new_tracked_changes", "new_untracked", "new_agent_branches", "new_worktrees"):
                for item in entry[key]:
                    print(f"  {key}: {entry['repo']}: {item}", file=sys.stderr)
            if entry["head_moved"]:
                print(f"  head_moved: {entry['repo']}: {entry['head_before']} -> {entry['head_after']}", file=sys.stderr)
        return 0 if report["verdict"] == "clean" else 1
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
