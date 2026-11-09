#!/bin/sh
# A stand-in for the `pixi` CLI so the integration tests run offline and fast.
# It understands only what the agent calls:
#   fake-pixi install --locked --manifest-path DIR/pixi.toml
#   fake-pixi run --locked --manifest-path DIR/pixi.toml <task>
# `healthcheck` runs DIR/healthcheck.sh and `start` runs DIR/app.sh.
cmd="$1"; shift
manifest=""; task=""
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest-path) manifest="$2"; shift 2 ;;
    --locked|--frozen) shift ;;
    *) task="$1"; shift ;;
  esac
done
dir=$(dirname "$manifest")
case "$cmd" in
  install) mkdir -p "$dir/.pixi" && touch "$dir/.pixi/installed"; exit 0 ;;
  run)
    cd "$dir" || exit 2
    case "$task" in
      healthcheck) exec sh ./healthcheck.sh ;;
      start) exec sh ./app.sh ;;
      *) echo "fake-pixi: unknown task $task" >&2; exit 2 ;;
    esac ;;
  *) echo "fake-pixi: unknown command $cmd" >&2; exit 2 ;;
esac
