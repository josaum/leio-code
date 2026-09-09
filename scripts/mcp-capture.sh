#!/usr/bin/env bash
# MITM wire capture for MCP stdio debugging: proxies between the client and
# the real launcher, teeing every JSON-RPC line (both directions) to a log.
# Point an mcpServers entry at this script to record the next connection.
# Override the log with MCP_CAPTURE_LOG; the err sidecar captures stderr.
set -euo pipefail
DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="${MCP_CAPTURE_LOG:-/tmp/leio-mcp-capture.log}"
REAL="$DIR/scripts/launch-stdio-mcp.sh"
note() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$1" >> "$LOG"; }

INFILE="$(mktemp)"
cleanup() {
  rm -f "$INFILE"
  note "-- capture ended --"
}
trap cleanup EXIT

# Record everything the client sends, forward it to the real server.
note "-- capture started ($(basename "$REAL")) --"
while IFS= read -r line; do
  note "IN  $line"
  printf '%s\n' "$line"
done | bash "$REAL" | while IFS= read -r line; do
  note "OUT $line"
  printf '%s\n' "$line"
done
