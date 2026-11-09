#!/usr/bin/env bash
# Run from this worktree inside nix develop, after nix build --out-link .runtime/result.
set -euo pipefail
scene="${1:?Pass the private scene manifest path}"
root="$PWD"
mkdir -p .runtime
cleanup() { systemctl --user stop chr-dawn-test.service >/dev/null 2>&1 || true; }
if systemctl --user is-active --quiet chr-dawn-test.service; then
    echo "The chr-dawn-test unit already exists; refusing to replace it." >&2
    exit 1
fi
trap cleanup EXIT
systemd-run --user --unit=chr-dawn-test --collect \
    --property=RuntimeMaxSec=180 --property="WorkingDirectory=$root" \
    --property="StandardOutput=append:$root/.runtime/srt-server.log" \
    --property="StandardError=append:$root/.runtime/srt-server.log" \
    "$root/.runtime/result/bin/chr-dawn" \
    --scene "$scene" --output 'srt://127.0.0.1:19002?mode=listener&latency=120000&listen_timeout=15000000' \
    --ledger .runtime/srt.jsonl --frames 150 --pace
for attempt in $(seq 1 50); do
    if ss -lun | grep -q '127.0.0.1:19002'; then break; fi
    sleep 0.1
done
systemctl --user is-active --quiet chr-dawn-test.service
ss -lun | grep '127.0.0.1:19002'
ffmpeg -nostdin -hide_banner -y -i 'srt://127.0.0.1:19002?mode=caller&latency=120000' \
    -c copy .runtime/srt.ts > .runtime/srt-receiver.log 2>&1
python tools/verify_dawn.py --scene "$scene" --output .runtime/srt.ts --ledger .runtime/srt.jsonl
echo "The loopback-only SRT stream decoded successfully; the test service is stopped on exit."
