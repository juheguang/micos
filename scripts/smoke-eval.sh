#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/micos-smoke-eval-$$"
trap 'rm -rf "$TMP_ROOT"' EXIT

mkdir -p "$TMP_ROOT"
cd "$ROOT"

OUTPUT="$(cargo run --quiet -- eval --cwd "$TMP_ROOT" 2>&1)"

echo "$OUTPUT" | grep -q "eval: 5/5 passed"
echo "$OUTPUT" | grep -q "denied_tool: passed"
echo "$OUTPUT" | grep -q "failing_tool_recovery: passed"
echo "$OUTPUT" | grep -q "compact_resume: passed"

echo "smoke-eval passed"
