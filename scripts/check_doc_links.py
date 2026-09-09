#!/usr/bin/env python3
"""Verify local Markdown links and heading fragments under LEIO Code.

Resolves hard-coded developer paths under ``.../example-workspace/leio-code`` and
``.../example-workspace`` against ``--leio-root`` and ``--workspace-root`` so links
still validate on GitHub Actions without a local developer home directory.
"""

from __future__ import annotations

import argparse
import html
import re
import sys
from pathlib import Path
from urllib.parse import unquote

# Authoritative monorepo path prefix used by older generated documentation links.
_WS_PREFIX = Path("/Users/josaum/projects/example-workspace")
_SKIP_PARTS = {"node_modules", "target", ".git", ".leio-code", "vendor"}
_LINK_PATTERNS = (
    re.compile(r"\]\(([^)]+)\)"),  # [text](url)
    re.compile(r"^\[[^\]]+\]:\s+(\S+)", re.MULTILINE),  # [ref]: url
    re.compile(r"\b(?:href|src)\s*=\s*['\"]([^'\"]+)['\"]", re.IGNORECASE),
)
_FENCE_OPEN = re.compile(r"^[ \t]{0,3}(`{3,}|~{3,})")
_ATX_HEADING = re.compile(r"^[ \t]{0,3}#{1,6}[ \t]+(.+?)\s*$")
_SETEXT_HEADING = re.compile(r"^[ \t]{0,3}(?:=+|-+)[ \t]*$")
_HTML_ANCHOR = re.compile(r"\b(?:id|name)\s*=\s*['\"]([^'\"]+)['\"]", re.IGNORECASE)
_HTML_COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)
_INLINE_CODE = re.compile(r"(`+)(.*?)\1", re.DOTALL)
_INLINE_LINK_LABEL = re.compile(r"!?\[([^\]]*)\]\([^)]*\)")
_REFERENCE_LINK_LABEL = re.compile(r"!?\[([^\]]*)\]\[[^\]]*\]")
_HTML_TAG = re.compile(r"<[^>]+>")
_URI_SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


def _destination(raw: str) -> str:
    """Return the URL portion without an optional Markdown link title."""
    target = raw.strip()
    if target.startswith("<"):
        closing = target.find(">")
        if closing >= 0:
            return target[1:closing].strip()
    return target.split(maxsplit=1)[0] if target else ""


def _local_parts(raw: str) -> tuple[str, str | None] | None:
    target = _destination(raw)
    if not target or target.startswith("//") or _URI_SCHEME.match(target):
        return None
    path_and_query, separator, fragment = target.partition("#")
    path_part = unquote(path_and_query.split("?", 1)[0])
    return path_part, unquote(fragment) if separator else None


def _normalize_inline_target(
    raw: str,
    md_path: Path,
    leio_root: Path,
    workspace_root: Path,
) -> Path | None:
    """Return the local filesystem target, or ``None`` for an external URL."""
    parts = _local_parts(raw)
    if parts is None:
        return None
    path_part, _fragment = parts
    if not path_part:
        return md_path.resolve()

    path = Path(path_part)
    try:
        if path.is_absolute():
            relative_workspace = path.relative_to(_WS_PREFIX)
            first = relative_workspace.parts[0] if relative_workspace.parts else ""
            if first == "leio-code":
                return (leio_root / Path(*relative_workspace.parts[1:])).resolve()
            return (workspace_root / relative_workspace).resolve()
    except ValueError:
        pass

    return (md_path.parent / path).resolve()


def _is_nested_worktree(path: Path, root: Path) -> bool:
    try:
        parts = path.relative_to(root).parts
    except ValueError:
        return False
    return any(parts[index : index + 2] == (".claude", "worktrees") for index in range(len(parts) - 1))


def _iter_markdown_files(root: Path) -> list[Path]:
    files: list[Path] = []
    for path in root.rglob("*.md"):
        if any(part in _SKIP_PARTS for part in path.parts):
            continue
        if _is_nested_worktree(path, root):
            continue
        files.append(path)
    return sorted(files)


def _without_fenced_code(body: str) -> str:
    """Blank fenced code blocks while preserving surrounding Markdown."""
    output: list[str] = []
    fence_character: str | None = None
    fence_length = 0
    for line in body.splitlines(keepends=True):
        if fence_character is None:
            match = _FENCE_OPEN.match(line)
            if match:
                fence_character = match.group(1)[0]
                fence_length = len(match.group(1))
                output.append("\n" if line.endswith("\n") else "")
            else:
                output.append(line)
            continue

        closing = re.match(
            rf"^[ \t]{{0,3}}{re.escape(fence_character)}{{{fence_length},}}[ \t]*(?:\n)?$",
            line,
        )
        if closing:
            fence_character = None
            fence_length = 0
        output.append("\n" if line.endswith("\n") else "")
    return "".join(output)


def _markdown_without_code(body: str) -> str:
    body = _without_fenced_code(body)
    body = _HTML_COMMENT.sub("", body)
    return _INLINE_CODE.sub("", body)


def _extract_links(body: str) -> list[str]:
    searchable = _markdown_without_code(body)
    found: list[str] = []
    for pattern in _LINK_PATTERNS:
        found.extend(match.group(1).strip() for match in pattern.finditer(searchable))
    return found


def _heading_text(markdown: str) -> str:
    text = re.sub(r"\s+#+\s*$", "", markdown.strip())
    text = _INLINE_LINK_LABEL.sub(r"\1", text)
    text = _REFERENCE_LINK_LABEL.sub(r"\1", text)
    text = _HTML_TAG.sub("", text)
    text = text.replace("\\", "")
    text = text.translate(str.maketrans("", "", "`*_~"))
    return html.unescape(text).strip()


def _github_slug(text: str) -> str:
    """Approximate GitHub's heading slug for repository documentation."""
    normalized = _heading_text(text).lower()
    characters = [character for character in normalized if character.isalnum() or character in {"-", "_"} or character.isspace()]
    return re.sub(r"\s", "-", "".join(characters))


def _markdown_anchors(body: str) -> set[str]:
    searchable = _without_fenced_code(body)
    anchors = set(_HTML_ANCHOR.findall(searchable))
    used_slugs: set[str] = set()
    lines = searchable.splitlines()

    def add_heading(text: str) -> None:
        base = _github_slug(text)
        if not base:
            return
        slug = base
        suffix = 1
        while slug in used_slugs:
            slug = f"{base}-{suffix}"
            suffix += 1
        used_slugs.add(slug)
        anchors.add(slug)

    for index, line in enumerate(lines):
        atx = _ATX_HEADING.match(line)
        if atx:
            add_heading(atx.group(1))
            continue
        if index > 0 and _SETEXT_HEADING.match(line) and lines[index - 1].strip():
            add_heading(lines[index - 1])
    return anchors


def _document_anchors(path: Path) -> set[str] | None:
    suffix = path.suffix.lower()
    if suffix in {".md", ".markdown"}:
        return _markdown_anchors(path.read_text(encoding="utf-8"))
    if suffix in {".htm", ".html"}:
        return set(_HTML_ANCHOR.findall(path.read_text(encoding="utf-8")))
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--leio-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Root of the leio-code repository (default: parent of this script)",
    )
    parser.add_argument(
        "--workspace-root",
        type=Path,
        default=None,
        help="Monorepo root (default: parent of --leio-root)",
    )
    args = parser.parse_args()
    leio_root: Path = args.leio_root.resolve()
    workspace_root: Path = (args.workspace_root or leio_root.parent).resolve()
    markdown_files = _iter_markdown_files(leio_root)

    issues: list[tuple[Path, str, Path, str]] = []
    anchor_cache: dict[Path, set[str] | None] = {}
    for md_path in markdown_files:
        text = md_path.read_text(encoding="utf-8")
        for raw in _extract_links(text):
            target = _normalize_inline_target(raw, md_path, leio_root, workspace_root)
            if target is None:
                continue
            if not target.exists():
                issues.append((md_path, raw, target, "target does not exist"))
                continue

            parts = _local_parts(raw)
            fragment = parts[1] if parts else None
            if not fragment:
                continue
            if target not in anchor_cache:
                anchor_cache[target] = _document_anchors(target)
            anchors = anchor_cache[target]
            if anchors is not None and fragment not in anchors:
                issues.append((md_path, raw, target, f"fragment #{fragment} not found"))

    if issues:
        print("check_doc_links: broken markdown targets:", file=sys.stderr)
        for source, raw, resolved, reason in issues:
            print(
                f"  {source.relative_to(leio_root)}: {raw!r} -> {resolved} ({reason})",
                file=sys.stderr,
            )
        return 1

    print(f"check_doc_links: ok ({len(markdown_files)} markdown files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
