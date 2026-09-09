#!/usr/bin/env bash
# Objective for `leio-harness integrate` / `gate` / `compare`.
#
# Contract: print exactly one OutcomeSnapshot JSON object to stdout —
#   {"metrics": {...}, "minimize": [...], "isotropy": null}
# — surrounding log lines are tolerated by the harness, but keep them on stderr.
# The harness runs this once on the baseline ref and once on the integrated tree,
# with cwd = the checkout under evaluation. Promotion happens only when at least
# one metric is strictly better and none is worse (verdict `Improved`).
#
# Bash 3 compatible (macOS). Copy, then replace the two collectors below with the
# commands that measure YOUR change. Every metric must be deterministic for a
# given tree — no timestamps, no network, no model calls.

set -u
REPO="${REPO:-$(pwd)}"
LEIO_CODE_BIN="${LEIO_CODE_BIN:-$HOME/.cargo/bin/leio-code}"

# --- collector 1: tests passed (higher is better) -----------------------------
# Replace with the scoped test command for the area under change. Examples:
#   pytest -q cartridges/<name>/tests 2>&1 | tail -1     -> "12 passed in 3.2s"
#   cargo test -p <crate> 2>&1 | grep -E '^test result'   -> "test result: ok. 66 passed; 0 failed"
tests_passed() {
  local out
  out="$(${OBJECTIVE_TEST_CMD:-echo "0 passed"} 2>/dev/null | tail -n 5)"
  echo "$out" | grep -Eo '[0-9]+ passed' | awk '{s+=$1} END {print s+0}'
}

# --- collector 2: LEIO doctor warnings (lower is better) ----------------------
doctor_warnings() {
  "$LEIO_CODE_BIN" --json --repo "$REPO" doctor all 2>/dev/null \
    | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin)
except Exception:
    print(0); raise SystemExit
w=d.get("warnings")
if isinstance(w,list): print(len(w))
else:
    n=0
    for s in d.get("suites",[]) or []:
        n+=len(s.get("warnings",[]) or [])
    print(n)'
}

TP="$(tests_passed)"; TP="${TP:-0}"
DW="$(doctor_warnings)"; DW="${DW:-0}"

echo "objective: tests_passed=$TP doctor_warnings=$DW (repo=$REPO)" >&2
printf '{"metrics":{"tests_passed":%s,"doctor_warnings":%s},"minimize":["doctor_warnings"],"isotropy":null}\n' "$TP" "$DW"
