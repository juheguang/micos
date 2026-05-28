#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/micos-smoke-memory-$$"
trap 'rm -rf "$TMP_ROOT"' EXIT

mkdir -p "$TMP_ROOT"

cd "$ROOT"

FIRST_OUTPUT="$(
  printf '/memory candidates refresh\n/memory candidates\n/exit\n' |
    MICOS_API_KEY=test cargo run --quiet -- chat --permission safe --cwd "$TMP_ROOT" --no-tui 2>&1
)"

echo "$FIRST_OUTPUT" | grep -q "memory candidate created"
echo "$FIRST_OUTPUT" | grep -q "memory candidates:"

CANDIDATE_PATH="$(find "$TMP_ROOT/.micos/memory/candidates" -name '*.toml' -type f | head -n 1)"
if [[ -z "${CANDIDATE_PATH}" ]]; then
  echo "missing memory candidate file" >&2
  exit 1
fi
CANDIDATE_ID="$(basename "$CANDIDATE_PATH" .toml)"

SECOND_OUTPUT="$(
  printf '/memory candidates\n/memory promote %s\n/context\n/exit\n' "$CANDIDATE_ID" |
    MICOS_API_KEY=test cargo run --quiet -- chat --permission safe --cwd "$TMP_ROOT" --no-tui 2>&1
)"

echo "$SECOND_OUTPUT" | grep -q "memory promoted"
echo "$SECOND_OUTPUT" | grep -q "pressure:"

ENTRY_PATH="$TMP_ROOT/.micos/memory/entries/$CANDIDATE_ID.toml"
if [[ ! -f "$ENTRY_PATH" ]]; then
  echo "missing promoted memory entry file" >&2
  exit 1
fi
grep -q 'status = "active"' "$ENTRY_PATH"

echo "smoke-memory ok"
