#!/usr/bin/env bash
# Driver for the run-leio-code skill: build leio-code and drive its CLI surface
# against a target repo, asserting exit codes + expected output markers.
#
# Usage:
#   .claude/skills/run-leio-code/smoke.sh [REPO]
#     REPO  repo to index/analyze (default: the leio-code crate itself — hermetic)
#
# Run from the leio-code crate root. Uses `cargo run --release` so it always
# drives a FRESH binary (the shared CARGO_TARGET_DIR=./target means a stray
# target/release/leio-code can be stale — see SKILL.md Gotchas).
set -uo pipefail

REPO="${1:-.}"
CRATE_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"   # .../leio-code
cd "$CRATE_DIR" || { echo "FAIL: cannot cd to crate dir $CRATE_DIR"; exit 1; }

pass=0 fail=0
run() { # run <label> <expected-substring> -- <leio args...>
  local label="$1" want="$2"; shift 3
  local out rc
  # progress/index logs go to stderr; keep them out of the captured output
  out="$(cargo run --release -q -- "$@" --repo "$REPO" 2>/dev/null)"; rc=$?
  if [ "$rc" -eq 0 ] && printf '%s' "$out" | grep -qiF "$want"; then
    echo "  PASS  $label"; pass=$((pass+1))
  else
    echo "  FAIL  $label (exit=$rc, wanted '$want')"
    printf '%s\n' "$out" | head -3 | sed 's/^/        /'
    fail=$((fail+1))
  fi
}

echo "==> build (fresh; ~2m cold, seconds warm)"
cargo build --release 2>&1 | tail -1

echo "==> drive leio-code CLI against: $REPO"
run "status"            "indexed"                       -- status
run "verify"            "doctors"                       -- verify
run "find symbol"       "matches for"                   -- find symbol encode_node_query_vectors
run "graph callers-of"  "callers"                       -- graph callers-of visit_node_entities
run "doctor repo-hygiene" "repo-hygiene:"               -- doctor repo-hygiene
run "audit (markdown)"  "Workspace Audit"               -- audit --format markdown
# JSON surface: with --json, stdout is clean machine-readable (logs are on stderr)
echo "==> json surface check (status --json | jq-less parse)"
if cargo run --release -q -- status --repo "$REPO" --json 2>/dev/null | python3 -c 'import sys,json;json.load(sys.stdin);print("  PASS  status --json parses")'; then
  pass=$((pass+1)); else echo "  FAIL  status --json"; fail=$((fail+1)); fi

echo "==> $pass passed, $fail failed"
[ "$fail" -eq 0 ]
