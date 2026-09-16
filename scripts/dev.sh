#!/usr/bin/env bash
# dev build + run loop for OpenProxy
# Usage:
#   ./scripts/dev.sh              # incremental debug build + run foreground (Ctrl+C to stop)
#   ./scripts/dev.sh detach       # build + run detached on 127.0.0.1:4625
#   ./scripts/dev.sh build        # only build, don't run
#   PORT=4626 ./scripts/dev.sh    # custom port
#   DATA_DIR=/tmp/openproxy-dev ./scripts/dev.sh # custom dev data dir
#   MODE=release ./scripts/dev.sh # release build (slower, optimized)
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${PORT:-4625}"
MODE="${1:-run}"
BUILD_MODE="${BUILD_MODE:-debug}"  # debug (incremental, fast) or release
DATA_DIR="${DATA_DIR:-$HOME/.openproxy-dev}"

BIN_DEBUG="target/debug/openproxy"
BIN_RELEASE="target/release/openproxy"
BIN="$BIN_DEBUG"
CARGO_ARGS=(build --bin openproxy)
if [[ "$BUILD_MODE" == "release" ]]; then
  BIN="$BIN_RELEASE"
  CARGO_ARGS=(build --release --bin openproxy)
fi

stop_dev_server() {
  # Stop only the server registered in the isolated dev data directory. Never
  # kill an arbitrary process on the port: production uses the same binary.
  if [[ -x "$BIN" ]]; then
    "$BIN" --data-dir "$DATA_DIR" server stop >/dev/null 2>&1 || true
  fi
}

build() {
  echo "== trunk build dashboard =="
  (cd dashboard && trunk build)
  echo "== cargo ${CARGO_ARGS[*]} =="
  # incremental by default; only rebuilds crates that changed
  # --bin openproxy avoids building tests/examples
  cargo "${CARGO_ARGS[@]}"
  echo "== built $BIN =="
  ls -lh "$BIN" | awk '{print $9, $5, $6, $7, $8}'
}

case "$MODE" in
  build)
    build
    echo "Build done. Run ./scripts/dev.sh to start."
    ;;
  detach)
    stop_dev_server
    build
    echo "== starting $BIN server start --port $PORT --detach --no-open =="
    "$BIN" --data-dir "$DATA_DIR" server start --detach --no-open --port "$PORT"
    echo "== status =="
    "$BIN" --data-dir "$DATA_DIR" --robot server status 2>&1 | head -n 20 || curl -sf "http://127.0.0.1:${PORT}/health" && echo "health ok"
    echo "Logs: tail -f $DATA_DIR/openproxy.log"
    echo "Stop: $BIN --data-dir $DATA_DIR server stop"
    ;;
  run|restart|"")
    stop_dev_server
    build
    echo "== starting $BIN server start --port $PORT (foreground, Ctrl+C to stop) =="
    echo "   Data:      $DATA_DIR"
    echo "   Dashboard: http://127.0.0.1:${PORT}"
    echo "   API:       http://127.0.0.1:${PORT}/v1  (Bearer \$OPENPROXY_API_KEY)"
    exec "$BIN" --data-dir "$DATA_DIR" server start --port "$PORT" --no-open
    ;;
  *)
    echo "Unknown mode: $MODE"
    echo "Usage: $0 [run|build|detach]  (default: run)"
    exit 1
    ;;
esac
