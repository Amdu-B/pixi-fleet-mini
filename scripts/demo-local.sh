#!/usr/bin/env bash
# Runs the whole story on one machine: an operator node plus three "robots"
# as separate processes, real Pixi, real conda packages.
#
#   1. roll out v1                      -> everyone converges
#   2. roll out v2 with a canary        -> canary first, then the rest
#   3. roll out a BROKEN v3 (canary)    -> canary rolls back, the others never try
#   4. kill the operator, roll out v4   -> spreads peer to peer
#
# Needs: pixi on PATH (or PIXI_BIN), Rust (or a prebuilt AGENT_BIN), and network
# access to conda-forge for the first install.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENT="${AGENT_BIN:-$ROOT/agent/target/release/agent}"
PIXI="${PIXI_BIN:-pixi}"
REL="$ROOT/sample-robot-app/releases"
DATA="${DEMO_DATA:-$ROOT/demo-data}"
PORT0="${BASE_PORT:-8000}"
OP="http://127.0.0.1:$PORT0"
R1="http://127.0.0.1:$((PORT0 + 1))"
ROBOTS="robot1,robot2,robot3"
PIDS=()

if [ ! -x "$AGENT" ]; then
  echo ">> building the agent (release)…"
  (cd "$ROOT/agent" && cargo build --release)
fi
command -v "$PIXI" >/dev/null || { echo "pixi not found. Install it from https://pixi.sh or set PIXI_BIN."; exit 1; }

cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

rm -rf "$DATA"; mkdir -p "$DATA"

start_agent() { # id port extra-args...
  local id="$1" port="$2"; shift 2
  "$AGENT" run --id "$id" --listen "127.0.0.1:$port" --data-dir "$DATA/$id" \
    --pixi-bin "$PIXI" --gossip-ms 1000 "$@" >"$DATA/$id.log" 2>&1 &
  PIDS+=($!)
}

step "starting an operator node and three robots (each robot only knows the operator)"
start_agent operator "$PORT0" --operator
OP_PID=${PIDS[0]}
sleep 1
for i in 1 2 3; do start_agent "robot$i" "$((PORT0 + i))" --peer "$OP"; done
sleep 3
echo "operator UI: $OP   (robot UIs: $R1 …)"
"$AGENT" status --to "$OP"

step "1/4  roll out v1-good to the whole fleet"
H1=$("$AGENT" publish "$REL/v1-good" --to "$OP")
"$AGENT" desire --to "$OP" --hash "$H1"
"$AGENT" wait --to "$OP" --hash "$H1" --robots "$ROBOTS" --timeout-secs 600
"$AGENT" status --to "$OP"

step "2/4  roll out v2-good with robot1 as canary"
H2=$("$AGENT" publish "$REL/v2-good" --to "$OP")
"$AGENT" desire --to "$OP" --hash "$H2" --canary robot1
"$AGENT" wait --to "$OP" --hash "$H2" --robots "$ROBOTS" --timeout-secs 600
"$AGENT" status --to "$OP"

step "3/4  roll out a BROKEN v3 with robot1 as canary"
H3=$("$AGENT" publish "$REL/v3-broken" --to "$OP")
"$AGENT" desire --to "$OP" --hash "$H3" --canary robot1
"$AGENT" wait --to "$OP" --hash "$H3" --robots robot1 --rejected --timeout-secs 600
sleep 3
"$AGENT" status --to "$OP"
echo "-> robot1 rejected v3 and kept v2. robot2/robot3 never tried it. Their events:"
curl -s "http://127.0.0.1:$((PORT0 + 2))/events" | python3 -c 'import sys,json; [print("   robot2:", e["message"]) for e in json.load(sys.stdin)[-2:]]'

step "4/4  kill the operator, then roll out v4-good through robot1"
kill "$OP_PID"; sleep 1
H4=$("$AGENT" publish "$REL/v4-good" --to "$R1")
"$AGENT" desire --to "$R1" --hash "$H4"
"$AGENT" wait --to "$R1" --hash "$H4" --robots "$ROBOTS" --timeout-secs 600
sleep 11   # long enough for the dead operator to show up as offline
"$AGENT" status --to "$R1"
echo "-> robot2/robot3 got v4 from their peers, no operator involved. Their events:"
curl -s "http://127.0.0.1:$((PORT0 + 3))/events" | python3 -c 'import sys,json; [print("   robot3:", e["message"]) for e in json.load(sys.stdin)[-4:]]'

step "what each robot's application printed last"
for i in 1 2 3; do printf 'robot%s: ' "$i"; tail -n1 "$DATA/robot$i/logs/app.log"; done

step "demo finished (logs in $DATA)"
