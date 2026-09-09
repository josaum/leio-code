#!/usr/bin/env bash
# SessionStart: inject the canonical LEIO skill. stdout is session context.
set -euo pipefail
ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
SKILL="${LEIO_CODE_SKILL:-$ROOT/skills/leio-code/SKILL.md}"
if [[ ! -f "$SKILL" ]]; then
  printf 'LEIO Code skill missing at %s\n' "$SKILL"
  exit 0
fi
python3 - "$SKILL" <<'PY'
from pathlib import Path
import sys

text = Path(sys.argv[1]).read_text()
if text.startswith("---"):
    parts = text.split("---", 2)
    if len(parts) >= 3:
        text = parts[2].lstrip("\n")
print(text, end="")
PY
