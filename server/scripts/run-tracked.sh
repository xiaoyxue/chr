#!/usr/bin/env bash
# Starts a tracked-camera simulation: chr-tracking-sim (capture and tracking),
# chr-server --tracked-scene and chr-monitor. With --tracking-worker the simulator is
# the camera only and chr-server's tracking worker (chr-tracking-sim-cli, a replay
# stand-in) supplies the cameras. Each runs on its own: a process that exits
# is restarted after a short delay while the others keep running. Ctrl-C stops all.
#
# Usage: server/scripts/run-tracked.sh --scene <json> --video <mp4> [--description <json>]
#            [--size <WxH>] [--srt <ip:port>] [--dir <path>] [--no-monitor] [--tracking-worker]
#            [--sim-arg <arg>]... [--monitor-arg <arg>]... [chr-server options]
#
#   --scene <json>    calibrated-video-v1 manifest: the server's scene and lens, the
#                     simulator's per-frame Ti/C/warp
#   --video <mp4>     The manifest's clip, played in a loop as the camera
#   --description <json>  Run chr-server with this render description instead of the
#                     manifest's scene; the simulator still replays the manifest's cameras.
#                     Add --asset-root <dir> to let clients load scenes (description.load)
#   --size <WxH>      Program output size: even, up to 7680x4320, with the camera's
#                     aspect ratio; chr-monitor previews at most 1920x1080 [default: 1280x720]
#   --srt <ip:port>   chr-monitor SRT listener [default: 127.0.0.1:9001]
#   --sim-arg <arg>   Pass one argument to chr-tracking-sim (repeatable), for example
#                     --sim-arg --tracking-delay-ms --sim-arg 40
#   --monitor-arg <arg>  Pass one argument to chr-monitor (repeatable)
#   --dir <path>      Directory for the socket, input ring and program tap, kept on exit
#                     [default: a private directory removed on exit]
#   --no-monitor      Do not start chr-monitor; start it separately with
#                     --tap <dir>/program.tap and the same --size/--fps
#   --tracking-worker chr-server hands every frame to its tracking worker, which
#                     replays the manifest's cameras; the simulator sends none
#
# The output rate is the manifest's fps. Run from the repository root inside
# `nix develop .#media`; on a non-NixOS NVIDIA host wrap this script with nixGL.
set -euo pipefail

scene=
video=
description=
size=1280x720
srt=127.0.0.1:9001
dir=
with_monitor=1
tracking_worker=
sim_args=()
monitor_args=()
server_args=()
while (($#)); do
  case $1 in
    --scene) scene=$2; shift 2 ;;
    --video) video=$2; shift 2 ;;
    --description) description=$2; shift 2 ;;
    --size) size=$2; shift 2 ;;
    --srt) srt=$2; shift 2 ;;
    --dir) dir=$2; shift 2 ;;
    --no-monitor) with_monitor=; shift ;;
    --tracking-worker) tracking_worker=1; shift ;;
    --sim-arg) sim_args+=("$2"); shift 2 ;;
    --monitor-arg) monitor_args+=("$2"); shift 2 ;;
    *) server_args+=("$1"); shift ;;
  esac
done
if [[ -z $scene || -z $video ]]; then
  echo "usage: $0 --scene <json> --video <mp4> [--size <WxH>] [--srt <ip:port>] [--dir <path>] [--no-monitor] [--sim-arg <arg>]... [--monitor-arg <arg>]... [chr-server options]" >&2
  exit 2
fi
fps=$(tr -d ' \n' <"$scene" | sed -n 's/.*"fps":\([0-9]*\).*/\1/p')

cargo build --locked --release -p chr-runtime -p chr-monitor \
  --features chr-runtime/gstreamer-media,chr-monitor/gstreamer-media
target=$(cargo metadata --locked --format-version 1 --no-deps |
  sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

# shellcheck source=server/scripts/supervise.sh
source "$(dirname "$0")/supervise.sh"
if [[ -n $dir ]]; then
  mkdir -p -m 700 "$dir"
  trap stop_supervised EXIT INT TERM
else
  dir=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/chr-tracked.XXXXXX")
  trap 'stop_supervised; rm -rf "$dir"' EXIT INT TERM
fi
echo "socket $dir/render.sock, input ring $dir/input.ring, program tap $dir/program.tap" >&2
program=(--tracked-scene "$scene")
if [[ -n $description ]]; then
  program=(--description "$description")
fi
if [[ -n $tracking_worker ]]; then
  program+=(--tracking-worker "$scene")
  sim_args+=(--no-tracking)
fi
supervise chr-server "$target/release/chr-server" "${program[@]}" --size "$size" --fps "$fps" \
  --socket "$dir/render.sock" --input-ring "$dir/input.ring" --monitor-tap "$dir/program.tap" \
  "${server_args[@]}"
if [[ -n $with_monitor ]]; then
  supervise chr-monitor "$target/release/chr-monitor" --tap "$dir/program.tap" --size "$size" \
    --fps "$fps" --srt "$srt" "${monitor_args[@]}"
fi
supervise chr-tracking-sim "$target/release/chr-tracking-sim" --scene "$scene" --video "$video" \
  --socket "$dir/render.sock" --input-ring "$dir/input.ring" "${sim_args[@]}"
wait
