#!/usr/bin/env bash
# Starts a tracked-camera simulation: chr-sdi-in (the capture input: the clip at its own
# rate, looped, sent as memfd frames over unixfd), chr-server --tracked-scene on its
# GStreamer core and chr-monitor. With --tracking-worker chr-server hands every frame
# to its tracking worker over vchao's VchaoFrame contract: the chr-tracking-sim-cli
# replay stand-in, or the --tracking-cli executable such as vchao's pitch-analysis.
# Without it chr-sdi-in also sends the manifest's recorded cameras as tracking.frame
# (chr-sdi-in --send-cameras). Each runs on its own: a process that exits is restarted
# after a short delay while the others keep running. Ctrl-C stops all.
#
# Usage: server/scripts/run-tracked.sh --scene <json> --video <file> [--description <json>]
#            [--size <WxH>] [--srt <ip:port>] [--dir <path>] [--no-monitor] [--no-audio]
#            [--tracking-worker] [--tracking-cli <path>] [--input-arg <arg>]...
#            [--monitor-arg <arg>]... [chr-server options]
#
#   --scene <json>    calibrated-video-v1 manifest: the server's scene, the stand-in's or
#                     the external cameras' per-frame Ti/C, lens and warp
#   --video <file>    The manifest's clip, played at its own rate in a loop as the camera
#   --description <json>  Run chr-server with this render description instead of the
#                     manifest's scene; the cameras still replay the manifest's.
#                     Add --asset-root <dir> to let clients load scenes (description.load)
#   --size <WxH>      Program output size: even, up to 4096x2160, with the camera's
#                     aspect ratio; chr-monitor previews at most 1920x1080 [default: 1280x720]
#   --srt <ip:port>   chr-monitor SRT listener [default: 127.0.0.1:9001]
#   --input-arg <arg> Pass one argument to chr-sdi-in (repeatable), for example
#                     --input-arg --tracking-delay-ms --input-arg 40 (without --tracking-worker)
#   --monitor-arg <arg>  Pass one argument to chr-monitor (repeatable)
#   --dir <path>      Directory for the sockets and the program tap, kept on exit
#                     [default: a private directory removed on exit]
#   --no-monitor      Do not start chr-monitor; start it separately with
#                     --tap <dir>/program.tap and the same --size/--fps
#   --no-audio        Neither send nor pass through the clip's audio
#   --tracking-worker chr-server hands every frame to its tracking worker; chr-sdi-in
#                     sends no cameras. Without --tracking-cli the worker is
#                     chr-tracking-sim-cli replaying the manifest's Ti and lens (no warps)
#   --tracking-cli <path>  Tracking worker executable instead of the stand-in, for
#                     example pitch-analysis (implies --tracking-worker). Give its own
#                     arguments as chr-server options --tracking-worker-arg <arg>, and
#                     --tracking-centre <x> <y> <z>; --tracking-threads <n> caps its
#                     CPU threads (OMP_NUM_THREADS)
#
# Other chr-server options pass through: --program-delay-ms <n> (the program delay D),
# --input-loss, --output-rate, the lens options --tracking-lens <auto|pinhole>,
# --tracking-lens-k <k1> <k2> <k3>, --tracking-lens-centre <x> <y>, --tracking-lens-scale
# <px> and --tracking-lens-support <4 numbers>.
#
# The output follows the clip's rate (the manifest's fps is chr-server's nominal rate).
# Run from the repository root inside `nix develop .#media`; on a non-NixOS NVIDIA host
# wrap this script with nixGL.
set -euo pipefail

scene=
video=
description=
size=1280x720
srt=127.0.0.1:9001
dir=
with_monitor=1
audio=1
tracking_worker=
tracking_cli=
input_args=()
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
    --no-audio) audio=; shift ;;
    --tracking-worker) tracking_worker=1; shift ;;
    --tracking-cli) tracking_worker=1; tracking_cli=$2; shift 2 ;;
    --input-arg) input_args+=("$2"); shift 2 ;;
    --monitor-arg) monitor_args+=("$2"); shift 2 ;;
    *) server_args+=("$1"); shift ;;
  esac
done
if [[ -z $scene || -z $video ]]; then
  echo "usage: $0 --scene <json> --video <file> [--size <WxH>] [--srt <ip:port>] [--dir <path>] [--no-monitor] [--no-audio] [--input-arg <arg>]... [--monitor-arg <arg>]... [chr-server options]" >&2
  exit 2
fi
# The manifest's rate: an integer (50) or a string ("30000/1001").
fps=$(tr -d ' \n' <"$scene" | sed -n 's/.*"fps":"\{0,1\}\([0-9./]*\).*/\1/p')

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
echo "control socket $dir/render.sock, capture socket $dir/capture.sock, program tap $dir/program.tap" >&2
program=(--tracked-scene "$scene")
if [[ -n $description ]]; then
  program=(--description "$description")
fi
if [[ -n $tracking_worker ]]; then
  program+=(--tracking-worker)
  if [[ -n $tracking_cli ]]; then
    program+=(--tracking-cli "$tracking_cli")
  else
    # The stand-in returns the manifest's recorded ideal-image Ti with its lens.
    program+=(--tracking-worker-arg --manifest --tracking-worker-arg "$scene")
  fi
else
  input_args+=(--send-cameras "$scene" --server-socket "$dir/render.sock")
fi
if [[ -z $audio ]]; then
  program+=(--no-audio)
  input_args+=(--no-audio)
fi
supervise chr-server "$target/release/chr-server" "${program[@]}" --size "$size" --fps "$fps" \
  --socket "$dir/render.sock" --capture-socket "$dir/capture.sock" \
  --monitor-tap "$dir/program.tap" "${server_args[@]}"
if [[ -n $with_monitor ]]; then
  supervise chr-monitor "$target/release/chr-monitor" --tap "$dir/program.tap" --size "$size" \
    --fps "$fps" --srt "$srt" "${monitor_args[@]}"
fi
supervise chr-sdi-in "$target/release/chr-sdi-in" --video "$video" --socket "$dir/capture.sock" \
  "${input_args[@]}"
wait
