#!/usr/bin/env bash
# Rolls a real ROS 2 Jazzy environment (RoboStack) out to two robots and shows
# the `talker` node running on each. First run downloads ~320 MB of packages.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENT="${AGENT_BIN:-$ROOT/agent/target/release/agent}"
PIXI="${PIXI_BIN:-pixi}"
DATA="${DEMO_DATA:-$ROOT/demo-data-ros2}"
PORT0="${BASE_PORT:-8100}"
OP="http://127.0.0.1:$PORT0"
PIDS=()

[ -x "$AGENT" ] || (cd "$ROOT/agent" && cargo build --release)
command -v "$PIXI" >/dev/null || { echo "pixi not found (https://pixi.sh)"; exit 1; }
trap 'for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true' EXIT INT TERM

rm -rf "$DATA"; mkdir -p "$DATA"
start() { local id="$1" port="$2"; shift 2
  "$AGENT" run --id "$id" --listen "127.0.0.1:$port" --data-dir "$DATA/$id" --pixi-bin "$PIXI" \
    --gossip-ms 1000 --install-timeout-secs 1800 "$@" >"$DATA/$id.log" 2>&1 &
  PIDS+=($!); }

start operator "$PORT0" --operator; sleep 1
start robot1 "$((PORT0 + 1))" --peer "$OP"
start robot2 "$((PORT0 + 2))" --peer "$OP"
sleep 3

H=$("$AGENT" publish "$ROOT/sample-robot-app/releases/ros2-jazzy" --to "$OP")
"$AGENT" desire --to "$OP" --hash "$H"
"$AGENT" wait --to "$OP" --hash "$H" --robots robot1,robot2 --timeout-secs 1800
"$AGENT" status --to "$OP"
sleep 3
for r in robot1 robot2; do echo "--- $r application log:"; tail -n 3 "$DATA/$r/logs/app.log"; done
