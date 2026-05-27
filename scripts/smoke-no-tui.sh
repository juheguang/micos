#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMPDIR="$(mktemp -d)"
SERVER="$TMPDIR/fake_responses.py"
PORT_FILE="$TMPDIR/port"
OUTPUT="$TMPDIR/output.txt"

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMPDIR"
}
trap cleanup EXIT

cat >"$SERVER" <<'PY'
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

SUMMARY = """## Primary Request and Intent
Continue smoke turn 4.

## Key Technical Concepts
Prompt governance, compact summary, permission modes, handoff.

## Files and Code Sections
No repo files changed by the smoke run.

## Errors and Fixes
None.

## Decisions Made
Use no-TUI smoke with a fake Responses API.

## Pending Tasks
None.

## Current Work
Smoke verification.

## Next Step
Report verified smoke status."""


def message(text):
    return {
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text}],
    }


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        instructions = body.get("instructions", "")
        if body.get("stream"):
            text = "stream ok"
            payload = json.dumps({
                "type": "response.completed",
                "response": {"output": [message(text)]},
            })
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            self.wfile.write(
                b"event: response.output_text.delta\n"
                + b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"stream ok\"}\n\n"
                + b"event: response.completed\n"
                + f"data: {payload}\n\n".encode()
            )
            return

        text = SUMMARY if "Compact task" in instructions else "ok"
        response = json.dumps({"output": [message(text)]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, *_):
        return


server = HTTPServer(("127.0.0.1", 0), Handler)
with open(sys.argv[1], "w") as port_file:
    port_file.write(str(server.server_port))
server.serve_forever()
PY

python3 "$SERVER" "$PORT_FILE" &
SERVER_PID="$!"

for _ in {1..50}; do
  [[ -s "$PORT_FILE" ]] && break
  sleep 0.1
done
if [[ ! -s "$PORT_FILE" ]]; then
  echo "fake Responses API failed to start" >&2
  exit 1
fi
PORT="$(cat "$PORT_FILE")"

(
  cd "$TMPDIR"
  printf '%s\n' \
    '/status' \
    '/context' \
    '/permission auto' \
    '/permission ask' \
    'smoke turn 0' \
    'smoke turn 1' \
    'smoke turn 2' \
    'smoke turn 3' \
    'smoke turn 4' \
    '/compact' \
    '/summary' \
    '/exit' \
  | MICOS_API_KEY=test MICOS_BASE_URL="http://127.0.0.1:${PORT}/responses" \
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

check_output "permission: auto"
check_output "permission: ask"
check_output "context compacted"
check_output "validation: passed"
check_output "## Primary Request and Intent"

SESSION_LOG="$(find "$TMPDIR/.micos/sessions" -name '*.jsonl' -print -quit)"
if [[ -z "$SESSION_LOG" ]] || ! grep -q '"type":"handoff_written"' "$SESSION_LOG"; then
  echo "missing handoff_written session event" >&2
  echo "--- smoke output ---" >&2
  cat "$OUTPUT" >&2
  [[ -n "${SESSION_LOG:-}" ]] && cat "$SESSION_LOG" >&2
  exit 1
fi

echo "smoke-no-tui passed"
