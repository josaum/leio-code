#!/usr/bin/env python3
"""Verify relative + repo-local absolute markdown links under leio-code (CI-friendly).

Resolves hard-coded developer paths under ``.../example-workspace/leio-code`` and
``.../example-workspace`` against ``--leio-root`` and ``--workspace-root`` so links
still validate on GitHub Actions without a local josaum home directory.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# Authoritative monorepo path prefix (see README / generated doc links).
_WS_PREFIX = Path("/Users/josaum/projects/example-workspace")


def _strip_fragment(url: str) -> str:
    return url.split("#", 1)[0]


def _normalize_inline_target(
    raw: str,
    md_path: Path,
    leio_root: Path,
    workspace_root: Path,
) -> Path | None:
    """Return filesystem path to verify, or None if skipped (external URL)."""
    t = raw.strip()
    if t.startswith("<") and t.endswith(">"):
        t = t[1:-1].strip()
    if not t or t.startswith(("#", "//")):
        return None
    lower = t.lower()
    if lower.startswith(("http://", "https://", "mailto:")):
        return None

    path_part = _strip_fragment(t)
    if not path_part:
        return None

    p = Path(path_part)

    # Repo-local absolute paths used in generated/README prose.
    try:
        if p.is_absolute():
            rel_ws = p.relative_to(_WS_PREFIX)
            first = rel_ws.parts[0] if rel_ws.parts else ""
            if first == "leio-code":
                return (leio_root / Path(*rel_ws.parts[1:])).resolve()
            return (workspace_root / rel_ws).resolve()
    except ValueError:
        pass

    # Relative to the markdown file.
    return (md_path.parent / path_part).resolve()


def _iter_markdown_files(root: Path) -> list[Path]:
    skip = {"node_modules", "target", ".git", "vendor"}
    out: list[Path] = []
    for p in root.rglob("*.md"):
        if any(part in skip for part in p.parts):
            continue
        out.append(p)
    return sorted(out)


_LINK_PATTERNS = (
    re.compile(r"\]\(([^)]+)\)"),  # [text](url)
    re.compile(r"^\[[^\]]+\]:\s+(\S+)", re.MULTILINE),  # [ref]: url
)
_IMG_PREFIX = re.compile(r"^!\[([^\]]*)\]")


def _extract_links(body: str) -> list[str]:
    found: list[str] = []
    for pat in _LINK_PATTERNS:
        for m in pat.finditer(body):
            found.append(m.group(1).strip())
    return found


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--leio-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Root of the leio-code repository (default: parent of this script)",
    )
    ap.add_argument(
        "--workspace-root",
        type=Path,
        default=None,
        help="Monorepo root (default: parent of --leio-root)",
    )
    args = ap.parse_args()
    leio_root: Path = args.leio_root.resolve()
    workspace_root: Path = (args.workspace_root or leio_root.parent).resolve()

    missing: list[tuple[Path, str, Path]] = []
    for md_path in _iter_markdown_files(leio_root):
        text = md_path.read_text(encoding="utf-8")
        for raw in _extract_links(text):
            target = _normalize_inline_target(raw, md_path, leio_root, workspace_root)
            if target is None:
                continue
            if target.exists():
                continue
            missing.append((md_path, raw, target))

    if missing:
        print("check_doc_links: broken markdown targets:", file=sys.stderr)
        for src, raw, resolved in missing:
            print(f"  {src.relative_to(leio_root)}: {raw!r} -> {resolved}", file=sys.stderr)
        return 1

    print(f"check_doc_links: ok ({len(list(_iter_markdown_files(leio_root)))} markdown files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
