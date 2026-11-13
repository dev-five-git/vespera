#!/usr/bin/env bash
# Smoke test of the rust-jni-demo GraalVM native image (vespera-bridge's
# shipped reachability metadata + Spring AOT hints, end to end).
#
#   scripts/native-image-smoke.sh <demo-app native executable> [port]
#
# Starts the executable twice -- platform threads, then
# spring.threads.virtual.enabled=true (SYNC dispatches then park on
# dispatchAsync, i.e. the CompletableFuture.complete JNI callback) -- and
# drives every dispatch path whose JNI lookups the metadata must cover:
#   DIRECT        GET  /health                    (dispatchDirect0)
#   overflow      GET  /stream/big                (DirectStash fetch/stream)
#   SYNC / ASYNC  POST /echo                      (dispatchBytes / dispatchAsync)
#   STREAMING     GET  /stream/events (SSE)       (Consumer.accept, OutputStream.write/flush)
#   BIDI          POST /echo/stream  (1 MiB, x3)  (InputStream.read/close)
#   422 hoisting  POST /documents/register {}
#   panic         POST /echo/panic                (500, panicked metadata)
# Exit 1 on the first failed check. Needs: bash, curl, head, cmp.
# Env: KEEP_LOG=<file> keeps the last application log (for crash reports).
set -euo pipefail

BIN=${1:?usage: native-image-smoke.sh <native executable> [port]}
PORT=${2:-18080}
BASE="http://127.0.0.1:$PORT"
WORK=$(mktemp -d)
PID=

cleanup() {
  if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; fi
  if [ -n "${KEEP_LOG:-}" ]; then cp "$WORK/app.log" "$KEEP_LOG" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  echo "---- application log (tail) ----" >&2
  tail -n 80 "$WORK/app.log" >&2 || true
  exit 1
}

expect() { # <name> <expected-status> <curl args...>
  local name=$1 want=$2
  shift 2
  local got
  got=$(curl -sS -o "$WORK/body" -w '%{http_code}' --max-time 30 "$@") || fail "$name: curl failed"
  [ "$got" = "$want" ] || fail "$name: HTTP $got (want $want): $(head -c 400 "$WORK/body")"
  echo "ok   $name ($got)"
}

body_contains() { # <name> <needle>
  grep -q -- "$2" "$WORK/body" || fail "$1: body lacks '$2': $(head -c 400 "$WORK/body")"
}

run_round() { # <label> [extra app args...]
  local label=$1
  shift
  echo "== $label"
  "$BIN" --server.port="$PORT" --logging.level.root=WARN "$@" >"$WORK/app.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 120); do
    if curl -fsS --max-time 2 "$BASE/health" >/dev/null 2>&1; then break; fi
    kill -0 "$PID" 2>/dev/null || fail "$label: the native image exited during startup"
    sleep 0.5
  done

  expect "GET /health (direct)" 200 "$BASE/health"
  [ "$(cat "$WORK/body")" = "ok" ] || fail "GET /health: body $(cat "$WORK/body")"

  expect "GET /stream/big (direct overflow)" 200 "$BASE/stream/big"
  [ "$(wc -c <"$WORK/body")" -gt 65536 ] || fail "GET /stream/big: short body"

  expect "POST /echo (sync)" 200 -H 'Content-Type: application/json' \
    --data-binary '{"hello":"native"}' "$BASE/echo"
  body_contains "POST /echo" '"hello":"native"'

  expect "GET /stream/events (sse)" 200 -H 'Accept: text/event-stream' "$BASE/stream/events"
  body_contains "GET /stream/events" 'data: event-2'

  # Repeated: a missing jni-crate binding only surfaced from the 2nd upload on
  # (its lookup recursed until the request thread's stack overflowed).
  head -c 1048576 /dev/urandom >"$WORK/upload"
  for i in 1 2 3; do
    curl -sS --max-time 60 -o "$WORK/echoed" -H 'Content-Type: application/octet-stream' \
      --data-binary "@$WORK/upload" "$BASE/echo/stream" || fail "POST /echo/stream #$i: curl failed"
    cmp -s "$WORK/upload" "$WORK/echoed" || fail "POST /echo/stream #$i: 1 MiB echo differs"
  done
  echo "ok   POST /echo/stream (bidirectional, 1 MiB, x3)"

  expect "POST /documents/register (422)" 422 -H 'Content-Type: application/json' \
    --data-binary '{"title":"","page_count":0}' "$BASE/documents/register"
  body_contains "POST /documents/register" "\"errors\""

  # Empty body (Content-Length: 0) so the smart resolver buffers it (SYNC).
  expect "POST /echo/panic (500)" 500 --data-binary '' "$BASE/echo/panic"

  kill -0 "$PID" 2>/dev/null || fail "$label: the native image died"
  kill "$PID"
  wait "$PID" 2>/dev/null || true
  PID=
}

run_round "platform threads"
run_round "virtual threads" --spring.threads.virtual.enabled=true
echo "native-image smoke: all checks passed"
