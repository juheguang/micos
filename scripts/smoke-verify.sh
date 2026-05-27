#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMPDIR="$(mktemp -d)"
OUTPUT="$TMPDIR/output.txt"

cleanup() {
  rm -rf "$TMPDIR"
}
trap cleanup EXIT

mkdir -p "$TMPDIR/.micos"
cat >"$TMPDIR/.micos/verify.toml" <<'TOML'
[[checks]]
name = "pwd"
command = "pwd"

[[checks]]
name = "missing"
command = "ls missing-file"
TOML

(
  cd "$TMPDIR"
  printf '%s\n' \
    '/verify pwd' \
    '/verify missing' \
    '/trace' \
    '/exit' \
  | MICOS_API_KEY=test \
      cargo run --manifest-path "$ROOT/Cargo.toml" --quiet -- chat --permission safe --model smoke --no-tui
) >"$OUTPUT" 2>&1

check_output() {
  local pattern="$1"
  if ! grep -q "$pattern" "$OUTPUT"; then
    echo "missing smoke output: $pattern" >&2
    echo "--- smoke output ---" >&2
    cat "$OUTPUT" >&2
    exit 1
  fi
}

check_output "verification: 1/1 passed"
check_output "pwd: passed"
check_output "missing: failed"
check_output "verification_finished: missing success=false"

SESSION_LOG="$(find "$TMPDIR/.micos/sessions" -name '*.jsonl' -print -quit)"
if [[ -z "$SESSION_LOG" ]] || ! grep -q '"type":"verification_finished"' "$SESSION_LOG"; then
  echo "missing verification_finished session event" >&2
  cat "$OUTPUT" >&2
  [[ -n "${SESSION_LOG:-}" ]] && cat "$SESSION_LOG" >&2
  exit 1
fi

echo "smoke-verify passed"
