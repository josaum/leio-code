#!/usr/bin/env bash
# LEIO-Harness validation gate. Rust-only; no example-workspace required.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${CARGO_TARGET_DIR:-target}/release/leio-harness"

echo "==> cargo fmt --check (leio-harness)"
cargo fmt --check -p leio-harness

echo "==> cargo clippy -D warnings (leio-harness)"
# --no-deps: vendored path dependencies (vendor/fca-fast-core) are frozen
# upstream snapshots. Unlike registry crates they do not get `--cap-lints
# allow`, so without this the gate fails on lint debt in code this repo must
# not edit, and never reaches leio-harness at all.
cargo clippy --no-deps -p leio-harness --all-targets -- -D warnings

echo "==> cargo test -p leio-harness"
cargo test -p leio-harness

echo "==> cargo test -p leio-harness (single-thread)"
cargo test -p leio-harness -- --test-threads=1

echo "==> cargo build --release -p leio-harness"
cargo build --release -p leio-harness

echo "==> bus selftest (publish/match/merge_gate/evolve)"
PORT=$((20000 + RANDOM % 20000))
"$BIN" bus selftest --port "$PORT"

echo "==> day echo drill + manifest"
DRILL="$(mktemp -d)"
mkdir -p "$DRILL/repo"
git -C "$DRILL/repo" init -qb main
git -C "$DRILL/repo" config user.email "harness@example.invalid"
git -C "$DRILL/repo" config user.name "harness"
echo x > "$DRILL/repo/README"
git -C "$DRILL/repo" add -A
git -C "$DRILL/repo" commit -qm init
cat > "$DRILL/day.json" <<EOF
{"goal":"validation","repo":"$DRILL/repo","worktreeRoot":"$DRILL/wt","outputDir":"$DRILL/runs","timeoutMs":30000,"parallel":true,"lanes":[{"agentId":"a","task":"t","argv":["/bin/echo","ok"]},{"agentId":"b","task":"t","argv":["/bin/echo","ok"]}]}
EOF
"$BIN" day --spec "$DRILL/day.json" >/dev/null
test -f "$DRILL/runs/manifest.json"

echo "==> LEIO-Harness validation gate PASSED"
