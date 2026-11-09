#!/usr/bin/env bash
# Run inside nix develop, after nix build --out-link .runtime/result.
set -euo pipefail
scene="${1:?Pass the private scene manifest path}"
root="$PWD"
unit=chr-dawn-disconnect.service
mkdir -p .runtime
if systemctl --user is-active --quiet "$unit"; then
    echo "The disconnect test unit already exists; refusing to replace it." >&2
    exit 1
fi
cleanup() {
    systemctl --user stop "$unit" >/dev/null 2>&1 || true
    systemctl --user reset-failed "$unit" >/dev/null 2>&1 || true
}
trap cleanup EXIT
systemd-run --user --unit="$unit" \
    --property=RuntimeMaxSec=45 --property="WorkingDirectory=$root" \
    --property="StandardOutput=append:$root/.runtime/disconnect-server.log" \
    --property="StandardError=append:$root/.runtime/disconnect-server.log" \
    "$root/.runtime/result/bin/chr-dawn" \
    --scene "$scene" \
    --output 'srt://127.0.0.1:19002?mode=listener&latency=120000&timeout=5000000&listen_timeout=15000000&linger=1' \
    --ledger .runtime/disconnect.jsonl --frames 1800 --loop --pace
for attempt in $(seq 1 50); do
    if ss -lun | grep -q '127.0.0.1:19002'; then break; fi
    sleep 0.1
done
systemctl --user is-active --quiet "$unit"
ffmpeg -nostdin -hide_banner -y -i 'srt://127.0.0.1:19002?mode=caller&latency=120000' \
    -frames:v 90 -c copy .runtime/disconnect-client.ts > .runtime/disconnect-client.log 2>&1
for attempt in $(seq 1 100); do
    if ! systemctl --user is-active --quiet "$unit"; then break; fi
    sleep 0.1
done
state="$(systemctl --user show "$unit" -p ActiveState --value)"
status="$(systemctl --user show "$unit" -p ExecMainStatus --value)"
result="$(systemctl --user show "$unit" -p Result --value)"
frames="$(wc -l < .runtime/disconnect.jsonl)"
if [ "$state" != failed ] || [ "$status" != 1 ] || [ "$result" != exit-code ] || [ "$frames" -ge 1800 ]; then
    echo "Early disconnect failed: state=$state exit=$status result=$result frames=$frames" >&2
    exit 1
fi
if ss -lun | grep -q '127.0.0.1:19002'; then
    echo "The disconnected sender still holds its socket." >&2
    exit 1
fi
echo "Early disconnect passed: sender exited 1 after $frames/1800 frames; localhost listener closed."
