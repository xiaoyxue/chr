#!/usr/bin/env bash
# Starts chr-server and chr-monitor together for development. Each runs on its own: a
# process that exits is restarted after a short delay while the other keeps running.
# Ctrl-C stops both.
#
# Usage: server/scripts/run-live.sh [--media] [--no-monitor] [--monitor-arg <arg>]...
#            [chr-server options]
#
#   --media              Build both with the gstreamer-media feature (run inside
#                        `nix develop .#media`), for --software-video, --srt and
#                        --encoder gstreamer.
#   --monitor-arg <arg>  Pass one argument to chr-monitor (repeatable), for example
#                        --monitor-arg --srt --monitor-arg 127.0.0.1:9001
#   --no-monitor         Do not start chr-monitor; start it separately with the same
#                        --size, --fps and tap path
#
# --size, --fps and --monitor-tap go to both processes (as --tap for chr-monitor);
# every other option goes to chr-server. Run from the repository root.
set -euo pipefail

features=()
server_args=()
monitor_args=()
with_monitor=1
while (($#)); do
  case $1 in
    --media)
      features=(--features chr-runtime/gstreamer-media,chr-monitor/gstreamer-media)
      shift
      ;;
    --no-monitor)
      with_monitor=
      shift
      ;;
    --monitor-arg)
      monitor_args+=("$2")
      shift 2
      ;;
    --size | --fps)
      server_args+=("$1" "$2")
      monitor_args+=("$1" "$2")
      shift 2
      ;;
    --monitor-tap)
      server_args+=("$1" "$2")
      monitor_args+=(--tap "$2")
      shift 2
      ;;
    *)
      server_args+=("$1")
      shift
      ;;
  esac
done

cargo build --locked --release -p chr-runtime -p chr-monitor "${features[@]}"
target=$(cargo metadata --locked --format-version 1 --no-deps |
  sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

# shellcheck source=server/scripts/supervise.sh
source "$(dirname "$0")/supervise.sh"
trap stop_supervised EXIT INT TERM
supervise chr-server "$target/release/chr-server" "${server_args[@]}"
if [[ -n $with_monitor ]]; then
  supervise chr-monitor "$target/release/chr-monitor" "${monitor_args[@]}"
fi
wait
