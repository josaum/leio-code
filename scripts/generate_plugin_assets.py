#!/usr/bin/env python3
"""Generate LEIO Code plugin UI assets."""

from __future__ import annotations

from pathlib import Path
from typing import Iterable

from PIL import Image, ImageDraw, ImageFilter, ImageFont


ROOT = Path(__file__).resolve().parents[1]
ASSETS_DIR = ROOT / "assets"

BG_TOP = (6, 23, 25, 255)
BG_BOTTOM = (16, 57, 56, 255)
PANEL = (242, 238, 231, 232)
PANEL_STRONG = (255, 250, 242, 245)
INK = (15, 33, 32, 255)
MUTED = (78, 103, 101, 255)
TEAL = (15, 118, 110, 255)
TEAL_DARK = (8, 81, 77, 255)
SAND = (228, 205, 165, 255)
ORANGE = (234, 120, 49, 255)
WHITE = (255, 255, 255, 255)
GRID = (255, 255, 255, 18)


def load_font(size: int, *, mono: bool = False, bold: bool = False) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    if mono:
        candidates = [
            "/System/Library/Fonts/Supplemental/Menlo.ttc",
            "/System/Library/Fonts/SFNSMono.ttf",
            "/Library/Fonts/Courier New.ttf",
        ]
    elif bold:
        candidates = [
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/System/Library/Fonts/Supplemental/Helvetica.ttc",
            "/Library/Fonts/Arial Bold.ttf",
        ]
    else:
        candidates = [
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Helvetica.ttc",
            "/Library/Fonts/Arial.ttf",
        ]

    for candidate in candidates:
        path = Path(candidate)
        if not path.exists():
            continue
        try:
            return ImageFont.truetype(str(path), size=size)
        except OSError:
            continue
    return ImageFont.load_default()


def lerp(a: int, b: int, t: float) -> int:
    return round(a + (b - a) * t)


def mix(c1: tuple[int, int, int, int], c2: tuple[int, int, int, int], t: float) -> tuple[int, int, int, int]:
    return tuple(lerp(a, b, t) for a, b in zip(c1, c2, strict=True))


def gradient_canvas(width: int, height: int) -> Image.Image:
    image = Image.new("RGBA", (width, height))
    draw = ImageDraw.Draw(image)
    for y in range(height):
        t = y / max(height - 1, 1)
        draw.line([(0, y), (width, y)], fill=mix(BG_TOP, BG_BOTTOM, t))
    return image


def add_blur_blob(image: Image.Image, center: tuple[int, int], radius: int, color: tuple[int, int, int, int]) -> Image.Image:
    overlay = Image.new("RGBA", image.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    x, y = center
    draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=color)
    overlay = overlay.filter(ImageFilter.GaussianBlur(max(radius // 3, 8)))
    return Image.alpha_composite(image, overlay)


def draw_grid(draw: ImageDraw.ImageDraw, width: int, height: int, step: int = 64) -> None:
    for x in range(0, width, step):
        draw.line([(x, 0), (x, height)], fill=GRID, width=1)
    for y in range(0, height, step):
        draw.line([(0, y), (width, y)], fill=GRID, width=1)


def draw_chip(draw: ImageDraw.ImageDraw, xy: tuple[int, int], text: str, *, fill: tuple[int, int, int, int], text_fill: tuple[int, int, int, int], font: ImageFont.ImageFont) -> None:
    x, y = xy
    left, top, right, bottom = draw.textbbox((0, 0), text, font=font)
    width = right - left + 34
    height = bottom - top + 18
    draw.rounded_rectangle((x, y, x + width, y + height), radius=height // 2, fill=fill)
    draw.text((x + 17, y + 9), text, fill=text_fill, font=font)


def draw_terminal_panel(
    draw: ImageDraw.ImageDraw,
    box: tuple[int, int, int, int],
    *,
    title: str,
    lines: Iterable[str],
    body_font: ImageFont.ImageFont,
    title_font: ImageFont.ImageFont,
) -> None:
    x1, y1, x2, y2 = box
    draw.rounded_rectangle(box, radius=28, fill=PANEL_STRONG, outline=(255, 255, 255, 40), width=2)
    draw.rounded_rectangle((x1 + 20, y1 + 18, x1 + 120, y1 + 42), radius=12, fill=(255, 255, 255, 150))
    for index, color in enumerate(((255, 99, 71, 220), (255, 193, 7, 220), (76, 175, 80, 220))):
        cx = x1 + 42 + index * 24
        cy = y1 + 30
        draw.ellipse((cx - 7, cy - 7, cx + 7, cy + 7), fill=color)
    draw.text((x1 + 150, y1 + 18), title, fill=INK, font=title_font)

    y = y1 + 72
    line_height = 44
    for line in lines:
        fill = TEAL_DARK if line.startswith("$") else INK
        if line.startswith("# "):
            fill = MUTED
        draw.text((x1 + 32, y), line, fill=fill, font=body_font)
        y += line_height
        if y > y2 - 48:
            break


def build_base(width: int, height: int) -> tuple[Image.Image, ImageDraw.ImageDraw]:
    image = gradient_canvas(width, height)
    image = add_blur_blob(image, (int(width * 0.82), int(height * 0.18)), int(min(width, height) * 0.18), (228, 205, 165, 92))
    image = add_blur_blob(image, (int(width * 0.16), int(height * 0.78)), int(min(width, height) * 0.2), (15, 118, 110, 90))
    image = add_blur_blob(image, (int(width * 0.56), int(height * 0.45)), int(min(width, height) * 0.12), (234, 120, 49, 55))
    draw = ImageDraw.Draw(image)
    draw_grid(draw, width, height)
    return image, draw


def build_icon() -> None:
    image, draw = build_base(512, 512)
    display_font = load_font(88, bold=True)
    chip_font = load_font(24, bold=True)

    draw.rounded_rectangle((84, 84, 428, 428), radius=40, fill=(255, 250, 242, 36), outline=(255, 255, 255, 42), width=2)
    nodes = [(170, 170), (344, 158), (256, 328)]
    for start, end in ((nodes[0], nodes[1]), (nodes[1], nodes[2]), (nodes[2], nodes[0])):
        draw.line([start, end], fill=(255, 255, 255, 120), width=10)
    for x, y in nodes:
        draw.ellipse((x - 24, y - 24, x + 24, y + 24), fill=SAND, outline=WHITE, width=4)
    draw.rounded_rectangle((146, 214, 366, 308), radius=28, fill=PANEL, outline=(255, 255, 255, 48), width=2)
    draw.text((180, 226), "LC", fill=INK, font=display_font)
    draw_chip(draw, (164, 370), "LEIO CODE", fill=ORANGE, text_fill=WHITE, font=chip_font)
    ASSETS_DIR.mkdir(parents=True, exist_ok=True)
    image.save(ASSETS_DIR / "icon.png")


def build_logo() -> None:
    image, draw = build_base(1600, 900)
    title_font = load_font(92, bold=True)
    subtitle_font = load_font(34)
    chip_font = load_font(24, bold=True)
    terminal_title_font = load_font(30, bold=True)
    terminal_font = load_font(28, mono=True)

    draw.text((96, 110), "LEIO Code", fill=WHITE, font=title_font)
    draw.text((96, 220), "Evidence-first code intelligence for arbitrary codebases.", fill=(233, 240, 238, 255), font=subtitle_font)
    draw.text((96, 272), "Profile-aware. Capability-aware. Still exact.", fill=(206, 219, 217, 255), font=subtitle_font)

    chips = ["status", "capabilities", "find", "explain", "graph", "export"]
    x = 96
    for chip in chips:
        draw_chip(draw, (x, 344), chip, fill=(255, 255, 255, 30), text_fill=WHITE, font=chip_font)
        left, _, right, _ = draw.textbbox((0, 0), chip, font=chip_font)
        x += (right - left) + 62

    draw_terminal_panel(
        draw,
        (820, 120, 1496, 780),
        title="MCP Surface",
        lines=[
            "$ leio_code_capabilities",
            "profile: generic",
            "find: symbol, env-var, redis-key, api-route, docker-service",
            "doctor suites: none configured",
            "# ",
            "$ leio_code_status",
            "Index: 1m ago, 14.5 MB",
            "workspace_profile: generic",
            "Capabilities: 5 find / 3 explain / 0 doctor suites",
            "# ",
            "$ leio_code_find kind=symbol needle=resolveLeioCodeRoot",
            "hit: leio-code/mcp/index.js:19",
        ],
        body_font=terminal_font,
        title_font=terminal_title_font,
    )
    image.save(ASSETS_DIR / "logo.png")


def build_screenshot(
    *,
    filename: str,
    title: str,
    subtitle: str,
    chips: list[str],
    terminal_title: str,
    lines: list[str],
) -> None:
    image, draw = build_base(1440, 900)
    title_font = load_font(64, bold=True)
    subtitle_font = load_font(28)
    chip_font = load_font(22, bold=True)
    terminal_title_font = load_font(28, bold=True)
    terminal_font = load_font(26, mono=True)

    draw.text((80, 88), title, fill=WHITE, font=title_font)
    draw.text((80, 170), subtitle, fill=(222, 234, 232, 255), font=subtitle_font)

    x = 80
    for chip in chips:
        draw_chip(draw, (x, 236), chip, fill=(255, 255, 255, 34), text_fill=WHITE, font=chip_font)
        left, _, right, _ = draw.textbbox((0, 0), chip, font=chip_font)
        x += (right - left) + 60

    draw.rounded_rectangle((80, 320, 470, 812), radius=32, fill=(255, 250, 242, 24), outline=(255, 255, 255, 32), width=2)
    draw.text((112, 372), "Why it matters", fill=SAND, font=load_font(30, bold=True))
    draw.text((112, 428), "Use the indexed LEIO surface when", fill=WHITE, font=subtitle_font)
    draw.text((112, 474), "a repo question is structural, cross-file,", fill=WHITE, font=subtitle_font)
    draw.text((112, 520), "deploy-aware, or runtime-aware.", fill=WHITE, font=subtitle_font)
    draw.text((112, 612), "Agent-first", fill=ORANGE, font=load_font(26, bold=True))
    draw.text((112, 652), "Exact evidence", fill=ORANGE, font=load_font(26, bold=True))
    draw.text((112, 692), "Shortest defensible answer", fill=ORANGE, font=load_font(26, bold=True))

    draw_terminal_panel(
        draw,
        (520, 114, 1360, 812),
        title=terminal_title,
        lines=lines,
        body_font=terminal_font,
        title_font=terminal_title_font,
    )
    image.save(ASSETS_DIR / filename)


def main() -> None:
    ASSETS_DIR.mkdir(parents=True, exist_ok=True)
    build_icon()
    build_logo()
    build_screenshot(
        filename="screenshot-1.png",
        title="Find exact ownership",
        subtitle="Symbols, env vars, Redis keys, routes, and services with file:line evidence, without assuming deploy topology exists.",
        chips=["symbol", "env-var", "redis-key", "api-route"],
        terminal_title="leio_code_find",
        lines=[
            "$ leio_code_find kind=symbol needle=resolveLeioCodeRoot",
            "summary: found 1 symbol match",
            "path: leio-code/mcp/index.js:19",
            "kind: function",
            "# ",
            "$ leio_code_find kind=api-route needle=/health",
            "hit: example-api/example/routers/health.py:18",
            "hit: leio-code/mcp/index.js:868",
        ],
    )
    build_screenshot(
        filename="screenshot-2.png",
        title="Check workspace capabilities",
        subtitle="The UI shows which families are meaningful for the current repo, and hides the rest unless the repo models them.",
        chips=["profile", "capabilities", "generic", "example"],
        terminal_title="leio_code_capabilities",
        lines=[
            "$ leio_code_capabilities",
            "profile: generic",
            "find: symbol, env-var, redis-key, api-route, docker-service",
            "explain: env-var, redis-key",
            "doctors: none configured",
            "# ",
            "$ leio_code_capabilities",
            "profile: example",
            "find: + deploy-target, cartridge",
            "doctors: 18 suites",
        ],
    )
    build_screenshot(
        filename="screenshot-3.png",
        title="Audit architectural drift",
        subtitle="Run CI-grade doctors only where the repo profile supports them, with warnings and lineage when they matter.",
        chips=["doctor all", "auth", "session", "route", "frontend"],
        terminal_title="leio_code_doctor",
        lines=[
            "$ leio_code_doctor kind=all",
            "profile: example",
            "deploy: green",
            "auth-brokering: green",
            "session-hot-state: green",
            "route-projection: green",
            "frontend-engine-client: green",
            "gateway-oar-boundary: warning",
            "action: align OCR route contract before deploy",
        ],
    )


if __name__ == "__main__":
    main()
