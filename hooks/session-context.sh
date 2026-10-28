#!/usr/bin/env bash
# SessionStart: inject the canonical LEIO skill. stdout is session context.
set -euo pipefail
ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
SKILL="${LEIO_CODE_SKILL:-$ROOT/skills/leio-code/SKILL.md}"
if [[ ! -f "$SKILL" ]]; then
  printf 'LEIO Code skill missing at %s\n' "$SKILL"
  exit 0
fi
awk '
  { line = $0; sub(/\r$/, "", line) }
  NR == 1 && line == "---" { in_fm = 1; next }
  in_fm && line == "---" { in_fm = 0; skip_blank = 1; next }
  in_fm { next }
  skip_blank && line == "" { next }
  { skip_blank = 0; print line }
' "$SKILL"
