#!/usr/bin/env bash
# Runs a Nix-built CUDA program, such as vchao's pitch-analysis, on a non-NixOS
# host: its wrappers look for the NVIDIA driver only in /run/opengl-driver/lib
# and replace LD_LIBRARY_PATH, so this preloads the driver's libcuda.so.1 from
# the inherited LD_LIBRARY_PATH (nixGLNvidia's, as stack.py and run-tracked.sh
# under nixGL provide). On NixOS it is not needed.
#
# Usage: server/scripts/with-cuda-driver.sh <program> [args]...
set -euo pipefail
IFS=: read -ra dirs <<<"${LD_LIBRARY_PATH:-}"
for dir in "${dirs[@]}"; do
  if [[ -n $dir && -e $dir/libcuda.so.1 ]]; then
    export LD_PRELOAD="$dir/libcuda.so.1${LD_PRELOAD:+:$LD_PRELOAD}"
    exec "$@"
  fi
done
if [[ ! -e /run/opengl-driver/lib/libcuda.so.1 ]]; then
  echo "with-cuda-driver.sh: no libcuda.so.1 on LD_LIBRARY_PATH; run under nixGLNvidia" >&2
  exit 1
fi
exec "$@"
