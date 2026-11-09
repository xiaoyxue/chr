#!/usr/bin/env bash
set -euo pipefail

scene="${1:?Usage: test_wgpu_srt.sh /private/scene.json}"
server="$(realpath "${2:-.runtime/result/bin/chr}")"
runtime="$PWD/.runtime"
mkdir -p -m 700 "$runtime"
chmod 700 "$runtime"
unit="chr-wgpu-test-$$"
if ss -H -lun 'sport = :19001' | grep -q .; then
    echo "UDP port 19001 is already in use; refusing to disturb its owner." >&2
    exit 1
fi
cleanup() {
    systemctl --user stop "$unit" >/dev/null 2>&1 || true
    systemctl --user reset-failed "$unit" >/dev/null 2>&1 || true
}
trap cleanup EXIT

systemd-run --user --unit="$unit" --collect \
    --property=RuntimeMaxSec=120 --property=TimeoutStopSec=10 \
    --property=UMask=0077 --property="WorkingDirectory=$PWD" \
    --property="StandardOutput=append:$runtime/srt-server.log" \
    --property="StandardError=append:$runtime/srt-server.log" \
    "$server" --scene "$scene" \
    --output 'srt://127.0.0.1:19001?mode=listener&latency=120000&timeout=5000000&listen_timeout=15000000&linger=1' \
    --frames 150 --status "$runtime/srt-status.json" --timecode-overlay

ready=false
for ((attempt = 0; attempt < 100; attempt++)); do
    pid="$(systemctl --user show "$unit" --property=MainPID --value)"
    if [ "$pid" != 0 ] && ss -H -lunp 'sport = :19001' | grep -Fq "pid=$pid,"; then
        ready=true
        break
    fi
    sleep 0.1
done
if [ "$ready" != true ]; then
    journalctl --user -u "$unit" --no-pager -n 25
    exit 1
fi

timeout 30 ffmpeg -hide_banner -nostdin -loglevel warning -y \
    -i 'srt://127.0.0.1:19001?mode=caller&latency=120000&timeout=5000000' \
    -c copy "$runtime/srt.ts" > "$runtime/srt-receiver.log" 2>&1
python - "$runtime/srt.ts" <<'PY'
import json
import sys
from fractions import Fraction

import av

count = 0
with av.open(sys.argv[1]) as container:
    first = None
    for frame in container.decode(video=0):
        time = frame.pts * frame.time_base
        if first is None:
            first = time
        assert time - first == Fraction(count, 30)
        count += 1
assert count == 150, count
print(json.dumps({"srt_frames": count, "pts_mismatches": 0, "timecode_overlay": True}))
PY
