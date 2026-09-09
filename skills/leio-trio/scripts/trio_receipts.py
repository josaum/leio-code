#!/usr/bin/env python3
"""Verify the receipts a trio report cites. Offline, stdlib only.

Usage:
    trio_receipts.py <report.md> [--repo /abs/repo ...] [--manifest /abs/manifest.json]
                     [--synthesis-heading Synthesis]

Checks (each is a finding in the JSON output):
  * leio_query_ids     every LEIO query id cited in the report (e.g. `context-1788477534952084000`,
                       `find_symbol-…`, `graph-…`, `doctor_<name>-…`) resolves to a `query_id` in
                       `<repo>/.leio-code/events/events.ndjson` for one of the --repo roots.
                       Matching is on the `query_id` field, never on the `@id` slug.
  * reference_ids          id shapes: `res:` / `snap:` 64 hex, `cl:` 24 hex; a receipt is cited together
                       with a snapshot. No MCP call is made — this is shape and co-presence only.
  * blocks             the three receipt blocks (LEIO Code / Reference Provider / Harness) are present.
  * captions           in the synthesis section, the share of sentences/bullets that carry a status
                       caption (Governed (signed), Interpretation (unsigned), Not known, unsigned host
                       reasoning, [signed]/[unsigned] markers). Reported as a ratio, not a verdict.
  * manifest           when a harness manifest is given or referenced and exists: SHA-256 of each
                       artifact `path` vs its recorded `sha256` (files that do not exist are listed,
                       not treated as errors).
  * harness_honesty    a lane table with runtime statuses, OR document-wide prose claiming lanes
                       executed and something merged ("six lanes ... merged automatically"), with
                       no DayReport/manifest/LaneOutcome reference anywhere is flagged — status
                       must come from a runtime artifact, not from a claim.

Exit codes: 0 = no fabrication signal; 1 = at least one cited LEIO query id did not resolve in the
given repos, or an Reference id is malformed; 2 = usage error. Everything else is informational so the
reader judges — the script counts, it does not grade prose.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import sys

QUERY_ID_RE = re.compile(
    r"(?<![A-Za-z0-9_.-])([a-z][a-z0-9_.-]*-\d{16,19})(?![A-Za-z0-9_.-])"
)
RES_RE = re.compile(r"(?<![A-Za-z0-9_])res:[A-Za-z0-9_-]*(?![A-Za-z0-9_-])")
SNAP_RE = re.compile(r"(?<![A-Za-z0-9_])snap:[A-Za-z0-9_-]*(?![A-Za-z0-9_-])")
CL_RE = re.compile(r"(?<![A-Za-z0-9_])cl:[A-Za-z0-9_-]*(?![A-Za-z0-9_-])")
EVENT_RE = re.compile(r"urn:reference:event:[A-Za-z0-9._:-]+")
CAPTION_MARKERS = (
    "governed (signed)", "interpretation (unsigned)", "not known", "unsigned host reasoning",
    "future outcome not observed", "[signed", "[unsigned", "(signed)", "(unsigned)",
    "**governed", "**interpretation", "**not known", "**unsigned",
)
HEADING_RE = re.compile(r"^(#{1,6})\s+(.*)$", re.M)
LANE_STATUS_WORDS = re.compile(r"\|\s*(ok|passed|failed|timed_out|canceled|infra_error|Committed|Aborted)\s*\|", re.I)
# a document-wide prose claim that a swarm executed and something merged, with
# no table — e.g. "six parallel lanes ... merged automatically into main"
LANE_COUNT_WORDS = r"(\d+|one|two|three|four|five|six|seven|eight|nine|ten)"
PROSE_EXECUTION_RE = re.compile(
    rf"\b{LANE_COUNT_WORDS}\s+(parallel\s+)?lanes\b|\blanes?\s+(ran|were run|executed|passed)\b"
    r"|\bmerged automatically\b|\bauto-?merged\b",
    re.I,
)
RUNTIME_ARTIFACT_RE = re.compile(
    r"manifest\.json|DayReport|LaneOutcome|ImprovementVerdict|not applicable|none ran|no lanes|none produced|no DayReport",
    re.I,
)


def load_query_ids(repo: str) -> set[str]:
    path = os.path.join(repo, ".leio-code", "events", "events.ndjson")
    ids: set[str] = set()
    if not os.path.isfile(path):
        return ids
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            qid = obj.get("query_id")
            if isinstance(qid, str):
                ids.add(qid)
    return ids


def sections(text: str) -> list[tuple[str, str]]:
    """Return [(heading, body)] in document order."""
    out: list[tuple[str, str]] = []
    matches = list(HEADING_RE.finditer(text))
    for index, match in enumerate(matches):
        start = match.end()
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        out.append((match.group(2).strip(), text[start:end]))
    return out


def caption_ratio(body: str) -> dict:
    units: list[str] = []
    for raw in body.splitlines():
        line = raw.strip()
        if not line or line.startswith("|") or line.startswith("```") or line.startswith("#"):
            continue
        # a lead-in line ("Corrected paragraph:", "Claim-by-claim verdict:") introduces
        # captioned units; it is not itself a claim and must not count against the ratio
        if line.rstrip("*_").endswith(":") and len(line.split()) <= 12:
            continue
        if line.startswith(("-", "*", "+")):
            units.append(line)
            continue
        # conservative sentence split for prose paragraphs
        for piece in re.split(r"(?<=[.!?])\s+(?=[A-Z\[*])", line):
            piece = piece.strip()
            if len(piece) > 12:
                units.append(piece)
    labelled = [u for u in units if any(marker in u.lower() for marker in CAPTION_MARKERS)]
    return {
        "units": len(units),
        "labelled": len(labelled),
        "ratio": round(len(labelled) / len(units), 3) if units else None,
        "unlabelled_examples": [u[:110] for u in units if u not in labelled][:5],
    }


def sha256_of(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def check_manifest(path: str) -> dict:
    try:
        manifest = json.load(open(path, "r", encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return {"path": path, "readable": False, "error": str(exc)}
    base = os.path.dirname(path)
    matched, mismatched, missing = [], [], []
    for entry in manifest.get("artifacts", []) or []:
        rel = entry.get("path")
        if not isinstance(rel, str):
            continue
        candidate = rel if os.path.isabs(rel) else os.path.join(base, rel)
        if not os.path.isfile(candidate):
            missing.append(rel)
            continue
        actual = sha256_of(candidate)
        (matched if actual == entry.get("sha256") else mismatched).append(rel)
    return {
        "path": path, "readable": True, "goal": manifest.get("goal"),
        "artifacts": len(manifest.get("artifacts", []) or []),
        "sha256_matched": len(matched), "sha256_mismatched": mismatched, "missing_files": missing,
    }


def split_reference_ids(text: str, pattern: re.Pattern, full_len: int) -> tuple[set[str], list[str], list[str]]:
    """Classify full, deliberately abbreviated, and malformed Reference ids."""
    matches = list(pattern.finditer(text))
    full, abbreviated, malformed = set(), [], []
    for match in matches:
        token = match.group(0)
        hexpart = token.split(":", 1)[1]
        if len(hexpart) == full_len and all(char in "0123456789abcdefABCDEF" for char in hexpart):
            full.add(token)
    for match in matches:
        token = match.group(0)
        hexpart = token.split(":", 1)[1]
        if token in full:
            continue
        tail = text[match.end():match.end() + 3]
        has_ellipsis = tail.startswith(("…", "..."))
        if hexpart and all(char in "0123456789abcdefABCDEF" for char in hexpart) and (
            any(candidate.startswith(token) for candidate in full) or has_ellipsis
        ):
            abbreviated.append(token)
        elif not hexpart and has_ellipsis:
            abbreviated.append(token)
        else:
            malformed.append(token)
    return full, abbreviated, malformed


def main(argv: list[str]) -> int:
    if not argv or argv[0].startswith("-"):
        print(__doc__, file=sys.stderr)
        return 2
    report_path = argv[0]
    repos: list[str] = []
    manifest_path: str | None = None
    synthesis_heading = "synthesis"
    it = iter(argv[1:])
    for arg in it:
        if arg == "--repo":
            repos.append(next(it, ""))
        elif arg == "--manifest":
            manifest_path = next(it, None)
        elif arg == "--synthesis-heading":
            synthesis_heading = next(it, "synthesis").lower()
        else:
            print(__doc__, file=sys.stderr)
            return 2
    try:
        text = open(report_path, "r", encoding="utf-8").read()
    except OSError as exc:
        print(json.dumps({"ok": False, "error": f"cannot read report: {exc}"}))
        return 2

    fabrication_signals: list[str] = []
    findings: dict = {"report": report_path}

    # 1. LEIO query ids
    cited = sorted(set(QUERY_ID_RE.findall(text)))
    known: dict[str, set[str]] = {repo: load_query_ids(repo) for repo in repos}
    resolved, unresolved = {}, []
    for qid in cited:
        homes = [repo for repo, ids in known.items() if qid in ids]
        if homes:
            resolved[qid] = homes
        else:
            unresolved.append(qid)
    findings["leio_query_ids"] = {
        "cited": len(cited), "resolved": len(resolved), "unresolved": unresolved,
        "repos_searched": {repo: len(ids) for repo, ids in known.items()},
    }
    if repos and unresolved:
        fabrication_signals.append(f"{len(unresolved)} cited LEIO query id(s) do not resolve in the given repos")
    if cited and not repos:
        findings["leio_query_ids"]["note"] = "pass --repo to resolve ids against events.ndjson"

    # 2. Reference ids — full ids must have the right length; a shorter token is an
    #    abbreviation only if it prefixes a full id in the same document (or is
    #    written with an ellipsis and prefixes one). Anything else is malformed.
    res_ids, res_abbr, res_bad = split_reference_ids(text, RES_RE, 64)
    snap_ids, snap_abbr, snap_bad = split_reference_ids(text, SNAP_RE, 64)
    cl_ids, cl_abbr, cl_bad = split_reference_ids(text, CL_RE, 24)
    malformed = res_bad + snap_bad + cl_bad
    findings["reference_ids"] = {
        "receipts": sorted(res_ids), "snapshots": sorted(snap_ids), "claims": len(cl_ids),
        "abbreviated": sorted(set(res_abbr + snap_abbr + cl_abbr)),
        "events": len(set(EVENT_RE.findall(text))), "malformed": malformed,
        "receipt_without_snapshot": bool(res_ids) and not snap_ids,
        "snapshot_without_receipt": bool(snap_ids) and not res_ids,
    }
    if malformed:
        fabrication_signals.append(f"{len(malformed)} malformed Reference id(s): {malformed[:4]}")

    # 3. Blocks
    heads = [h.lower() for h, _ in sections(text)]
    findings["blocks"] = {
        "leio": any("leio code" in h for h in heads),
        "reference": any("reference" in h for h in heads),
        "harness": any("harness" in h for h in heads),
        "synthesis": any(synthesis_heading in h for h in heads),
        "drift": any("drift" in h or "retroaliment" in h for h in heads),
    }

    # 4. Captions in synthesis
    synth = [body for h, body in sections(text) if synthesis_heading in h.lower()]
    findings["captions"] = caption_ratio("\n".join(synth)) if synth else {"units": 0, "labelled": 0, "ratio": None, "note": "no synthesis section found"}

    # 5. Manifest
    if not manifest_path:
        match = re.search(r"(/[^\s`'\"]+/manifest\.json)", text)
        manifest_path = match.group(1) if match else None
    if manifest_path and os.path.isfile(manifest_path):
        findings["manifest"] = check_manifest(manifest_path)
    else:
        findings["manifest"] = {"path": manifest_path, "present": False}

    # 6. Harness honesty — a lane table (or, failing that, prose claiming lanes
    # executed and something merged) with no runtime artifact behind it,
    # anywhere in the document, not only under a heading that says "Harness".
    # Real write-ups don't always organize into the template's sections, and a
    # fabricated-swarm claim is exactly the kind of prose that skips headings.
    harness_bodies = [body for h, body in sections(text) if "harness" in h.lower()]
    harness_text = "\n".join(harness_bodies) or text
    has_status_rows = bool(LANE_STATUS_WORDS.search(harness_text))
    has_prose_claim = bool(PROSE_EXECUTION_RE.search(text))
    has_runtime_ref = bool(RUNTIME_ARTIFACT_RE.search(harness_text))
    has_runtime_ref_anywhere = bool(RUNTIME_ARTIFACT_RE.search(text))
    findings["harness_honesty"] = {
        "lane_status_rows": has_status_rows,
        "prose_execution_claim": has_prose_claim,
        "runtime_artifact_referenced": has_runtime_ref,
        "runtime_artifact_referenced_anywhere": has_runtime_ref_anywhere,
        "flag": (has_status_rows or has_prose_claim) and not has_runtime_ref_anywhere,
    }

    ok = not fabrication_signals
    payload = {"ok": ok, "fabrication_signals": fabrication_signals, "findings": findings}
    print(json.dumps(payload, indent=2))
    q = findings["leio_query_ids"]
    c = findings["captions"]
    print(
        f"trio receipts [{'OK' if ok else 'SIGNAL'}] leio ids {q['resolved']}/{q['cited']} resolved · reference receipts {len(res_ids)} snapshots {len(snap_ids)} claims {len(cl_ids)} · captions {c.get('labelled')}/{c.get('units')} · blocks {findings['blocks']}",
        file=sys.stderr,
    )
    for signal in fabrication_signals:
        print(f"  SIGNAL {signal}", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
